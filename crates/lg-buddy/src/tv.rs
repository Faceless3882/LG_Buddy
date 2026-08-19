use std::env;
use std::error::Error;
#[cfg(unix)]
use std::ffi::CString;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::auth::{
    resolve_bscpylgtv_auth_context_from_env, resolve_config_owner, AuthContextError,
    BscpylgtvAuthContext, SystemUser,
};
use crate::config::{HdmiInput, MacAddress, TvPlatform};
use crate::platform_access_token::{PlatformAccessTokenStore, PlatformAccessTokenStoreError};
use crate::web_os::{WebOsEndpoint, WebOsTvClient};
use crate::wol::{WakeOnLanError, WakeOnLanSender};

pub const DEFAULT_BSCPYLGTV_COMMAND_PATH: &str = "/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand";
pub const OLED_BRIGHTNESS_MIN: u8 = 0;
pub const OLED_BRIGHTNESS_MAX: u8 = 100;
const DEFAULT_WEBOS_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_WEBOS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandOutput {
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    fn stdout(&self) -> &str {
        &self.stdout
    }

    fn stderr(&self) -> &str {
        &self.stderr
    }

    fn combined_output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (false, false) => format!("{}{}", self.stdout, self.stderr),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (true, true) => String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvOperation {
    ReadInput,
    SetInput,
    ReadOledBrightness,
    SetOledBrightness,
    PowerOff,
    BlankScreen,
    UnblankScreen,
}

impl TvOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadInput => "read input",
            Self::SetInput => "set input",
            Self::ReadOledBrightness => "read OLED brightness",
            Self::SetOledBrightness => "set OLED brightness",
            Self::PowerOff => "power off",
            Self::BlankScreen => "blank screen",
            Self::UnblankScreen => "unblank screen",
        }
    }
}

impl fmt::Display for TvOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvErrorKind {
    Transport,
    Authentication,
    Rejected,
    InvalidResponse,
    ScreenUnblankSubstateMismatch,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvError {
    operation: TvOperation,
    kind: TvErrorKind,
    detail: String,
}

impl fmt::Display for TvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not {}: {}", self.operation, self.detail)
    }
}

impl Error for TvError {}

impl TvError {
    pub fn operation(&self) -> TvOperation {
        self.operation
    }

    pub fn kind(&self) -> TvErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn new(
        operation: TvOperation,
        kind: TvErrorKind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            kind,
            detail: detail.into(),
        }
    }

    pub fn indicates_screen_unblank_substate_mismatch(&self) -> bool {
        self.kind == TvErrorKind::ScreenUnblankSubstateMismatch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OledBrightnessParseError {
    value: String,
}

impl OledBrightnessParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for OledBrightnessParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid OLED brightness `{}`; expected an integer from {} to {}",
            self.value, OLED_BRIGHTNESS_MIN, OLED_BRIGHTNESS_MAX
        )
    }
}

impl Error for OledBrightnessParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OledBrightness(u8);

impl OledBrightness {
    pub const DEFAULT: Self = Self(50);

    pub fn new(value: u8) -> Result<Self, OledBrightnessParseError> {
        if value <= OLED_BRIGHTNESS_MAX {
            Ok(Self(value))
        } else {
            Err(OledBrightnessParseError::new(value.to_string()))
        }
    }

    pub fn parse(value: &str) -> Result<Self, OledBrightnessParseError> {
        match value.parse::<i64>() {
            Ok(parsed)
                if parsed >= i64::from(OLED_BRIGHTNESS_MIN)
                    && parsed <= i64::from(OLED_BRIGHTNESS_MAX) =>
            {
                Ok(Self(parsed as u8))
            }
            _ => Err(OledBrightnessParseError::new(value)),
        }
    }

    pub fn as_percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for OledBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub trait TvClient {
    fn current_input(&self) -> Result<CurrentInput, TvError>;
    fn oled_brightness(&self) -> Result<OledBrightness, TvError>;
    fn set_input(&self, input: HdmiInput) -> Result<(), TvError>;
    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError>;
    fn power_off(&self) -> Result<(), TvError>;
    fn blank_screen(&self) -> Result<(), TvError>;
    fn unblank_screen(&self) -> Result<(), TvError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TvClientBuildOptions {
    command_timeout: Option<Duration>,
}

impl TvClientBuildOptions {
    pub(crate) fn production() -> Self {
        Self {
            command_timeout: None,
        }
    }

    pub(crate) fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = Some(timeout);
        self
    }
}

#[derive(Debug)]
pub enum TvClientBuildError {
    AuthContext(AuthContextError),
    TokenStore(PlatformAccessTokenStoreError),
}

impl fmt::Display for TvClientBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthContext(error) => write!(f, "{error}"),
            Self::TokenStore(error) => write!(f, "{error}"),
        }
    }
}

impl Error for TvClientBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AuthContext(error) => Some(error),
            Self::TokenStore(error) => Some(error),
        }
    }
}

pub(crate) enum SelectedTvClient {
    Bscpylgtv(BscpylgtvCommandClient<UserScopedBscpylgtvCommandLauncher>),
    WebOs(Box<WebOsTvClient>),
}

impl TvClient for SelectedTvClient {
    fn current_input(&self) -> Result<CurrentInput, TvError> {
        match self {
            Self::Bscpylgtv(client) => client.current_input(),
            Self::WebOs(client) => client.current_input(),
        }
    }

    fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        match self {
            Self::Bscpylgtv(client) => client.oled_brightness(),
            Self::WebOs(client) => client.oled_brightness(),
        }
    }

    fn set_input(&self, input: HdmiInput) -> Result<(), TvError> {
        match self {
            Self::Bscpylgtv(client) => client.set_input(input),
            Self::WebOs(client) => client.set_input(input),
        }
    }

    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        match self {
            Self::Bscpylgtv(client) => client.set_oled_brightness(brightness),
            Self::WebOs(client) => client.set_oled_brightness(brightness),
        }
    }

    fn power_off(&self) -> Result<(), TvError> {
        match self {
            Self::Bscpylgtv(client) => client.power_off(),
            Self::WebOs(client) => client.power_off(),
        }
    }

    fn blank_screen(&self) -> Result<(), TvError> {
        match self {
            Self::Bscpylgtv(client) => client.blank_screen(),
            Self::WebOs(client) => client.blank_screen(),
        }
    }

    fn unblank_screen(&self) -> Result<(), TvError> {
        match self {
            Self::Bscpylgtv(client) => client.unblank_screen(),
            Self::WebOs(client) => client.unblank_screen(),
        }
    }
}

pub(crate) fn build_tv_client(
    config_path: &Path,
    tv_ip: Ipv4Addr,
    platform: TvPlatform,
    options: TvClientBuildOptions,
) -> Result<SelectedTvClient, TvClientBuildError> {
    match platform {
        TvPlatform::Bscpylgtv => {
            let auth_context = resolve_bscpylgtv_auth_context_from_env(config_path)
                .map_err(TvClientBuildError::AuthContext)?;
            let mut client = BscpylgtvCommandClient::from_env(tv_ip)
                .with_auth_context(auth_context)
                .with_launcher(UserScopedBscpylgtvCommandLauncher);
            if let Some(timeout) = options.command_timeout {
                client = client.with_command_timeout(timeout);
            }
            Ok(SelectedTvClient::Bscpylgtv(client))
        }
        TvPlatform::LgWebOs => {
            let owner =
                resolve_config_owner(config_path).map_err(TvClientBuildError::AuthContext)?;
            let token_store = PlatformAccessTokenStore::for_primary_profile(config_path, owner)
                .map_err(TvClientBuildError::TokenStore)?;
            let response_timeout = options
                .command_timeout
                .unwrap_or(DEFAULT_WEBOS_RESPONSE_TIMEOUT);
            Ok(SelectedTvClient::WebOs(Box::new(WebOsTvClient::new(
                WebOsEndpoint::wss(tv_ip),
                DEFAULT_WEBOS_CONNECT_TIMEOUT,
                response_timeout,
                token_store,
            ))))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentInput {
    Hdmi(HdmiInput),
    Other(String),
}

impl CurrentInput {
    pub fn from_raw(value: String) -> Self {
        match HdmiInput::from_app_id(&value) {
            Some(input) => Self::Hdmi(input),
            None => Self::Other(value),
        }
    }

    pub fn is_hdmi(&self, input: HdmiInput) -> bool {
        matches!(self, Self::Hdmi(current) if *current == input)
    }
}

impl fmt::Display for CurrentInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hdmi(input) => write!(f, "{}", input.as_str()),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvDevice<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C> TvDevice<'a, C> {
    pub fn new(client: &'a C, tv_ip: Ipv4Addr) -> Self {
        Self { client, tv_ip }
    }
}

impl<'a, C: TvClient> TvDevice<'a, C> {
    pub fn input(&self) -> TvInput<'a, C> {
        TvInput {
            client: self.client,
        }
    }

    pub fn screen(&self) -> TvScreen<'a, C> {
        TvScreen {
            client: self.client,
        }
    }

    pub fn picture(&self) -> TvPicture<'a, C> {
        TvPicture {
            client: self.client,
        }
    }

    pub fn power(&self) -> TvPower<'a, C> {
        TvPower {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvInput<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvInput<'a, C> {
    pub fn current(&self) -> Result<CurrentInput, TvError> {
        self.client.current_input()
    }

    pub fn set(&self, input: HdmiInput) -> Result<(), TvError> {
        self.client.set_input(input)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvScreen<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvScreen<'a, C> {
    pub fn blank(&self) -> Result<(), TvError> {
        self.client.blank_screen()
    }

    pub fn unblank(&self) -> Result<(), TvError> {
        self.client.unblank_screen()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPicture<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvPicture<'a, C> {
    pub fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        self.client.oled_brightness()
    }

    pub fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        self.client.set_oled_brightness(brightness)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPower<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvPower<'a, C> {
    pub fn wake<W: WakeOnLanSender>(
        &self,
        sender: &W,
        tv_mac: &MacAddress,
    ) -> Result<(), WakeOnLanError> {
        sender.send_magic_packet_to(tv_mac, self.tv_ip)
    }

    pub fn off(&self) -> Result<(), TvError> {
        self.client.power_off()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BscpylgtvInvocation {
    program: PathBuf,
    args: Vec<String>,
    launch_identity: Option<SystemUser>,
    timeout: Option<Duration>,
}

impl BscpylgtvInvocation {
    pub fn new(
        program: impl Into<PathBuf>,
        args: Vec<String>,
        launch_identity: Option<SystemUser>,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            launch_identity,
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn launch_identity(&self) -> Option<&SystemUser> {
        self.launch_identity.as_ref()
    }

    pub fn launch_user(&self) -> Option<&str> {
        self.launch_identity().map(SystemUser::username)
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BscpylgtvLaunchResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl BscpylgtvLaunchResult {
    pub fn new(status: Option<i32>, stdout: String, stderr: String) -> Self {
        Self {
            status,
            stdout,
            stderr,
        }
    }

    pub fn status(&self) -> Option<i32> {
        self.status
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub fn is_success(&self) -> bool {
        self.status == Some(0)
    }
}

pub trait BscpylgtvCommandLauncher {
    fn run(&self, invocation: &BscpylgtvInvocation) -> io::Result<BscpylgtvLaunchResult>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DirectBscpylgtvCommandLauncher;

impl BscpylgtvCommandLauncher for DirectBscpylgtvCommandLauncher {
    fn run(&self, invocation: &BscpylgtvInvocation) -> io::Result<BscpylgtvLaunchResult> {
        if let Some(user) = invocation.launch_user() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("direct launcher cannot run `bscpylgtvcommand` as user `{user}`"),
            ));
        }

        let mut command = Command::new(invocation.program());
        command.args(invocation.args());
        let output = run_command_with_optional_timeout(command, invocation.timeout())?;

        Ok(BscpylgtvLaunchResult::new(
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UserScopedBscpylgtvCommandLauncher;

impl BscpylgtvCommandLauncher for UserScopedBscpylgtvCommandLauncher {
    fn run(&self, invocation: &BscpylgtvInvocation) -> io::Result<BscpylgtvLaunchResult> {
        let mut command = Command::new(invocation.program());
        command.args(invocation.args());

        if let Some(identity) = invocation.launch_identity() {
            configure_command_for_identity(&mut command, identity)?;
        }

        let output = run_command_with_optional_timeout(command, invocation.timeout())?;

        Ok(BscpylgtvLaunchResult::new(
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

#[derive(Debug)]
pub struct BscpylgtvCommandClient<L = DirectBscpylgtvCommandLauncher> {
    tv_ip: Ipv4Addr,
    command_path: PathBuf,
    command_args: Vec<String>,
    auth_context: BscpylgtvAuthContext,
    launcher: L,
    command_timeout: Option<Duration>,
}

impl BscpylgtvCommandClient<DirectBscpylgtvCommandLauncher> {
    pub fn new(tv_ip: Ipv4Addr, command_path: impl Into<PathBuf>) -> Self {
        Self {
            tv_ip,
            command_path: command_path.into(),
            command_args: Vec::new(),
            auth_context: BscpylgtvAuthContext::default(),
            launcher: DirectBscpylgtvCommandLauncher,
            command_timeout: None,
        }
    }

    pub fn with_args<I, S>(
        tv_ip: Ipv4Addr,
        command_path: impl Into<PathBuf>,
        command_args: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            tv_ip,
            command_path: command_path.into(),
            command_args: command_args.into_iter().map(Into::into).collect(),
            auth_context: BscpylgtvAuthContext::default(),
            launcher: DirectBscpylgtvCommandLauncher,
            command_timeout: None,
        }
    }

    pub fn from_env(tv_ip: Ipv4Addr) -> Self {
        match env::var_os("LG_BUDDY_BSCPYLGTV_COMMAND") {
            Some(path) => Self::new(tv_ip, PathBuf::from(path)),
            None => Self::new(tv_ip, DEFAULT_BSCPYLGTV_COMMAND_PATH),
        }
    }
}

impl<L> BscpylgtvCommandClient<L> {
    pub fn with_launcher<T>(self, launcher: T) -> BscpylgtvCommandClient<T> {
        BscpylgtvCommandClient {
            tv_ip: self.tv_ip,
            command_path: self.command_path,
            command_args: self.command_args,
            auth_context: self.auth_context,
            launcher,
            command_timeout: self.command_timeout,
        }
    }

    pub fn with_auth_context(mut self, auth_context: BscpylgtvAuthContext) -> Self {
        self.auth_context = auth_context;
        self
    }

    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = Some(timeout);
        self
    }

    pub fn command_path(&self) -> &Path {
        &self.command_path
    }

    pub fn tv_ip(&self) -> Ipv4Addr {
        self.tv_ip
    }

    pub fn command_args(&self) -> &[String] {
        &self.command_args
    }

    pub fn auth_context(&self) -> &BscpylgtvAuthContext {
        &self.auth_context
    }

    fn build_invocation(
        &self,
        operation: &'static str,
        extra_args: &[&str],
    ) -> BscpylgtvInvocation {
        let mut args = self.command_args.clone();

        if let Some(key_file_path) = self.auth_context.key_file_path() {
            args.push("-p".to_string());
            args.push(key_file_path.to_string_lossy().into_owned());
        }

        args.push(self.tv_ip.to_string());
        args.push(operation.to_string());
        args.extend(extra_args.iter().map(|arg| arg.to_string()));

        let invocation = BscpylgtvInvocation::new(
            self.command_path.clone(),
            args,
            self.auth_context.owner().cloned(),
        );

        match self.command_timeout {
            Some(timeout) => invocation.with_timeout(timeout),
            None => invocation,
        }
    }
}

fn run_command_with_optional_timeout(
    mut command: Command,
    timeout: Option<Duration>,
) -> io::Result<Output> {
    let Some(timeout) = timeout else {
        return command.output();
    };

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let started = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("bscpylgtvcommand timed out after {timeout:?}"),
            ));
        }

        let remaining = timeout.saturating_sub(elapsed);
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UserScopedLaunchPlan {
    UseCurrentIdentity,
    DropPrivileges {
        username: String,
        uid: u32,
        gid: u32,
    },
}

fn build_user_scoped_launch_plan(
    current_uid: u32,
    identity: &SystemUser,
) -> io::Result<UserScopedLaunchPlan> {
    if current_uid == identity.uid() {
        return Ok(UserScopedLaunchPlan::UseCurrentIdentity);
    }

    if current_uid == 0 {
        return Ok(UserScopedLaunchPlan::DropPrivileges {
            username: identity.username().to_string(),
            uid: identity.uid(),
            gid: identity.gid(),
        });
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "cannot run `bscpylgtvcommand` as user `{}` from uid `{current_uid}`",
            identity.username()
        ),
    ))
}

fn configure_command_for_identity(command: &mut Command, identity: &SystemUser) -> io::Result<()> {
    command.env("HOME", identity.home());
    command.env("USER", identity.username());
    command.env("LOGNAME", identity.username());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        match build_user_scoped_launch_plan(current_euid(), identity)? {
            UserScopedLaunchPlan::UseCurrentIdentity => return Ok(()),
            UserScopedLaunchPlan::DropPrivileges { username, uid, gid } => {
                let username = CString::new(username).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "cannot switch to user `{}` because the username contains a NUL byte",
                            identity.username()
                        ),
                    )
                })?;

                // Configure the child process to run with the owner's primary uid/gid and groups.
                unsafe {
                    command.pre_exec(move || {
                        if libc::initgroups(username.as_ptr(), gid) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        if libc::setgid(gid) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        if libc::setuid(uid) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = identity;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TV helper user scoping is only supported on Unix platforms",
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn current_euid() -> u32 {
    unsafe { libc::geteuid() }
}

impl<L: BscpylgtvCommandLauncher> BscpylgtvCommandClient<L> {
    fn run_command(
        &self,
        tv_operation: TvOperation,
        operation: &'static str,
        extra_args: &[&str],
    ) -> Result<CommandOutput, TvError> {
        let invocation = self.build_invocation(operation, extra_args);
        let output = self.launcher.run(&invocation).map_err(|source| {
            TvError::new(
                tv_operation,
                TvErrorKind::Transport,
                format!("failed to run `{operation}`: {source}"),
            )
        })?;

        let rendered = CommandOutput::new(output.stdout().to_string(), output.stderr().to_string());

        if output.is_success() {
            Ok(rendered)
        } else {
            let combined = rendered.combined_output();
            let status = output
                .status()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string());
            let mut detail = format!("`{operation}` failed with status {status}");
            if !combined.trim().is_empty() {
                detail.push_str(": ");
                detail.push_str(combined.trim_end());
            }
            let kind = if tv_operation == TvOperation::UnblankScreen
                && (rendered.stderr().contains("errorCode': '-102'")
                    || rendered.stdout().contains("errorCode': '-102'"))
            {
                TvErrorKind::ScreenUnblankSubstateMismatch
            } else {
                TvErrorKind::Rejected
            };
            Err(TvError::new(tv_operation, kind, detail))
        }
    }
}

impl<L: BscpylgtvCommandLauncher> TvClient for BscpylgtvCommandClient<L> {
    fn current_input(&self) -> Result<CurrentInput, TvError> {
        let output = self.run_command(TvOperation::ReadInput, "get_input", &[])?;
        last_non_empty_line(output.stdout())
            .map(CurrentInput::from_raw)
            .ok_or_else(|| {
                TvError::new(
                    TvOperation::ReadInput,
                    TvErrorKind::InvalidResponse,
                    invalid_command_output_detail(
                        "get_input",
                        &output,
                        "expected a non-empty line in stdout",
                    ),
                )
            })
    }

    fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        let output =
            self.run_command(TvOperation::ReadOledBrightness, "get_picture_settings", &[])?;
        parse_backlight(output.stdout()).ok_or_else(|| {
            TvError::new(
                TvOperation::ReadOledBrightness,
                TvErrorKind::InvalidResponse,
                invalid_command_output_detail(
                    "get_picture_settings",
                    &output,
                    "expected a backlight value in stdout",
                ),
            )
        })
    }

    fn set_input(&self, input: HdmiInput) -> Result<(), TvError> {
        self.run_command(TvOperation::SetInput, "set_input", &[input.as_str()])
            .map(|_| ())
    }

    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        let backlight = format!("{{\"backlight\": {brightness}}}");
        self.run_command(
            TvOperation::SetOledBrightness,
            "set_settings",
            &["picture", backlight.as_str()],
        )
        .map(|_| ())
    }

    fn power_off(&self) -> Result<(), TvError> {
        self.run_command(TvOperation::PowerOff, "power_off", &[])
            .map(|_| ())
    }

    fn blank_screen(&self) -> Result<(), TvError> {
        self.run_command(TvOperation::BlankScreen, "turn_screen_off", &[])
            .map(|_| ())
    }

    fn unblank_screen(&self) -> Result<(), TvError> {
        self.run_command(TvOperation::UnblankScreen, "turn_screen_on", &[])
            .map(|_| ())
    }
}

fn invalid_command_output_detail(
    command: &'static str,
    output: &CommandOutput,
    message: &'static str,
) -> String {
    let mut detail = format!("invalid output from `{command}`: {message}");
    let combined = output.combined_output();
    if !combined.trim().is_empty() {
        detail.push_str(": ");
        detail.push_str(combined.trim_end());
    }
    detail
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::to_string)
}

fn parse_backlight(output: &str) -> Option<OledBrightness> {
    for token in output.split([',', '{', '}', '\n']) {
        let token = token.trim();
        if !(token.starts_with("'backlight'") || token.starts_with("\"backlight\"")) {
            continue;
        }

        let (_, value) = token.split_once(':')?;
        let parsed = value
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .parse::<u8>()
            .ok()?;
        return OledBrightness::new(parsed).ok();
    }

    None
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        build_tv_client, build_user_scoped_launch_plan, current_euid, BscpylgtvCommandClient,
        BscpylgtvCommandLauncher, BscpylgtvInvocation, BscpylgtvLaunchResult, CommandOutput,
        CurrentInput, OledBrightness, SelectedTvClient, TvClient, TvClientBuildOptions, TvDevice,
        TvErrorKind, TvOperation, UserScopedBscpylgtvCommandLauncher, UserScopedLaunchPlan,
        DEFAULT_BSCPYLGTV_COMMAND_PATH,
    };
    use crate::auth::{BscpylgtvAuthContext, SystemUser};
    use crate::config::{HdmiInput, MacAddress, TvPlatform};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use std::cell::RefCell;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::Duration;
    use support::{ExecutableScript, MockBscpylgtv, TestConfigFile};

    #[test]
    fn oled_brightness_accepts_boundary_values() {
        let minimum = OledBrightness::parse("0").expect("minimum brightness should parse");
        let maximum = OledBrightness::parse("100").expect("maximum brightness should parse");

        assert_eq!(minimum.as_percent(), 0);
        assert_eq!(maximum.as_percent(), 100);
        assert_eq!(minimum.to_string(), "0");
        assert_eq!(OledBrightness::DEFAULT.as_percent(), 50);
    }

    #[test]
    fn oled_brightness_rejects_invalid_values() {
        for value in ["-1", "101", "abc", "50.5"] {
            let err = OledBrightness::parse(value).expect_err("invalid brightness should fail");
            assert!(err
                .to_string()
                .contains("expected an integer from 0 to 100"));
        }

        assert!(OledBrightness::new(101).is_err());
    }

    #[test]
    fn default_client_uses_expected_command_path() {
        let client =
            BscpylgtvCommandClient::new(ip("192.168.1.42"), DEFAULT_BSCPYLGTV_COMMAND_PATH);
        assert_eq!(
            client.command_path(),
            Path::new(DEFAULT_BSCPYLGTV_COMMAND_PATH)
        );
        assert!(client.command_args().is_empty());
        assert_eq!(client.auth_context(), &BscpylgtvAuthContext::default());
    }

    #[test]
    fn combined_output_preserves_stdout_and_stderr() {
        let output = CommandOutput::new("hello\n".to_string(), "world\n".to_string());
        assert_eq!(output.combined_output(), "hello\nworld\n");
    }

    #[test]
    fn centralized_factory_maps_each_platform_to_its_client() {
        let config = TestConfigFile::new("tv-client-factory");
        config.write_sample("HDMI_2");
        let tv_ip = ip("192.0.2.42");

        let bscpylgtv = build_tv_client(
            config.path(),
            tv_ip,
            TvPlatform::Bscpylgtv,
            TvClientBuildOptions::production(),
        )
        .expect("build bscpylgtv TV client");
        assert!(matches!(bscpylgtv, SelectedTvClient::Bscpylgtv(_)));

        let webos = build_tv_client(
            config.path(),
            tv_ip,
            TvPlatform::LgWebOs,
            TvClientBuildOptions::production(),
        )
        .expect("build native webOS TV client");
        assert!(matches!(webos, SelectedTvClient::WebOs(_)));
    }

    #[test]
    fn centralized_factory_applies_command_timeout_to_native_responses() {
        let config = TestConfigFile::new("tv-client-native-timeout");
        config.write_sample("HDMI_2");
        let command_timeout = Duration::from_secs(3);

        let webos = build_tv_client(
            config.path(),
            ip("192.0.2.42"),
            TvPlatform::LgWebOs,
            TvClientBuildOptions::production().with_command_timeout(command_timeout),
        )
        .expect("build native webOS TV client");

        let SelectedTvClient::WebOs(client) = webos else {
            panic!("lg_webos should select the native webOS client");
        };
        assert_eq!(client.response_timeout(), command_timeout);
    }

    #[test]
    fn get_input_uses_last_non_empty_stdout_line() {
        let mock = MockBscpylgtv::new("tv-get-input");
        mock.queue_success("get_input", "\nignored\ncom.webos.app.hdmi2\n");

        let client = client_for_mock(&mock, ip("192.168.1.42"));
        let input = client.current_input().expect("get_input should succeed");

        assert_eq!(input, CurrentInput::Hdmi(HdmiInput::Hdmi2));
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "192.168.1.42".to_string(),
                "get_input".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn get_input_rejects_empty_output() {
        let mock = MockBscpylgtv::new("tv-get-input-empty");
        mock.queue_success("get_input", "");

        let client = client_for_mock(&mock, ip("192.168.1.42"));
        let err = client
            .current_input()
            .expect_err("empty output should fail");

        assert_eq!(err.operation(), TvOperation::ReadInput);
        assert_eq!(err.kind(), TvErrorKind::InvalidResponse);
        assert!(err.detail().contains("expected a non-empty line in stdout"));
    }

    #[test]
    fn set_input_passes_expected_arguments() {
        let mock = MockBscpylgtv::new("tv-set-input");
        let client = client_for_mock(&mock, ip("10.0.0.5"));
        client
            .set_input(HdmiInput::Hdmi3)
            .expect("set_input should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "set_input".to_string(),
                vec!["HDMI_3".to_string()],
            )]
        );
    }

    #[test]
    fn set_oled_brightness_passes_expected_arguments() {
        let mock = MockBscpylgtv::new("tv-set-brightness");
        let client = client_for_mock(&mock, ip("10.0.0.5"));
        client
            .set_oled_brightness(brightness(65))
            .expect("set_oled_brightness should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "set_settings".to_string(),
                vec!["picture".to_string(), "{\"backlight\": 65}".to_string()],
            )]
        );
        assert_eq!(mock.state_snapshot().backlight, 65);
    }

    #[test]
    fn get_oled_brightness_reads_backlight_from_picture_settings() {
        let mock = MockBscpylgtv::new("tv-get-brightness");
        mock.set_backlight(72);
        let client = client_for_mock(&mock, ip("10.0.0.5"));

        let brightness = client
            .oled_brightness()
            .expect("get_oled_brightness should succeed");

        assert_eq!(brightness.as_percent(), 72);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "get_picture_settings".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn get_oled_brightness_rejects_missing_backlight_value() {
        let mock = MockBscpylgtv::new("tv-get-brightness-invalid");
        mock.queue_success("get_picture_settings", "{'contrast': 85}\n");
        let client = client_for_mock(&mock, ip("10.0.0.5"));

        let err = client
            .oled_brightness()
            .expect_err("missing backlight should fail");

        assert_eq!(err.operation(), TvOperation::ReadOledBrightness);
        assert_eq!(err.kind(), TvErrorKind::InvalidResponse);
        assert!(err
            .detail()
            .contains("expected a backlight value in stdout"));
    }

    #[test]
    fn tv_device_maps_hdmi_inputs_to_typed_values() {
        let mock = MockBscpylgtv::new("tv-device-current-hdmi");
        mock.set_input("HDMI_4");

        let client = client_for_mock(&mock, ip("10.0.0.7"));
        let tv = TvDevice::new(&client, ip("10.0.0.7"));
        let current = tv.input().current().expect("current input should parse");

        assert_eq!(current, CurrentInput::Hdmi(HdmiInput::Hdmi4));
    }

    #[test]
    fn tv_device_preserves_non_hdmi_inputs() {
        let mock = MockBscpylgtv::new("tv-device-current-other");
        mock.queue_success("get_input", "com.webos.app.youtube\n");

        let client = client_for_mock(&mock, ip("10.0.0.9"));
        let tv = TvDevice::new(&client, ip("10.0.0.9"));
        let current = tv.input().current().expect("current input should parse");

        assert_eq!(
            current,
            CurrentInput::Other("com.webos.app.youtube".to_string())
        );
    }

    #[test]
    fn tv_screen_blank_uses_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-screen-blank");
        let client = client_for_mock(&mock, ip("10.0.0.11"));
        let tv = TvDevice::new(&client, ip("10.0.0.11"));
        tv.screen().blank().expect("screen blank should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.11".to_string(),
                "turn_screen_off".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn tv_picture_set_oled_brightness_uses_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-picture-brightness");
        let client = client_for_mock(&mock, ip("10.0.0.12"));
        let tv = TvDevice::new(&client, ip("10.0.0.12"));
        tv.picture()
            .set_oled_brightness(brightness(40))
            .expect("brightness set should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.12".to_string(),
                "set_settings".to_string(),
                vec!["picture".to_string(), "{\"backlight\": 40}".to_string()],
            )]
        );
    }

    #[test]
    fn tv_picture_reads_oled_brightness_via_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-picture-read-brightness");
        mock.set_backlight(33);
        let client = client_for_mock(&mock, ip("10.0.0.13"));
        let tv = TvDevice::new(&client, ip("10.0.0.13"));

        let brightness = tv
            .picture()
            .oled_brightness()
            .expect("brightness read should succeed");

        assert_eq!(brightness.as_percent(), 33);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.13".to_string(),
                "get_picture_settings".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn tv_power_wake_uses_wake_on_lan_sender() {
        let client = BscpylgtvCommandClient::new(ip("10.0.0.15"), DEFAULT_BSCPYLGTV_COMMAND_PATH);
        let tv = TvDevice::new(&client, ip("10.0.0.15"));
        let sender = RecordingWakeOnLanSender::default();
        let mac = parse_mac("01:23:45:67:89:ab");

        tv.power()
            .wake(&sender, &mac)
            .expect("wake on lan should succeed");

        assert_eq!(sender.calls(), vec![(mac, Some(ip("10.0.0.15")))]);
    }

    #[test]
    fn command_failures_preserve_diagnostics_without_exposing_command_output() {
        let mock = MockBscpylgtv::new("tv-command-failure");
        mock.queue_error("turn_screen_on", 7, "failure stderr\n");
        let client = client_for_mock(&mock, ip("10.0.0.8"));
        let err = client
            .unblank_screen()
            .expect_err("turn_screen_on should fail");

        assert_eq!(err.operation(), TvOperation::UnblankScreen);
        assert_eq!(err.kind(), TvErrorKind::Rejected);
        assert!(err.detail().contains("failed with status 7"));
        assert!(err.detail().contains("failure stderr"));
    }

    #[test]
    fn command_timeout_stops_slow_helper() {
        let script = ExecutableScript::new(
            "tv-command-timeout",
            "slow-bscpylgtvcommand",
            "#!/bin/sh\nsleep 5\n",
        );
        let client = BscpylgtvCommandClient::new(ip("10.0.0.8"), script.path())
            .with_command_timeout(Duration::from_millis(100));

        let err = client
            .current_input()
            .expect_err("slow command should time out");

        assert_eq!(err.operation(), TvOperation::ReadInput);
        assert_eq!(err.kind(), TvErrorKind::Transport);
        assert!(
            err.detail().contains("timed out after"),
            "unexpected error detail: {}",
            err.detail()
        );
    }

    #[test]
    fn invocation_places_key_file_override_before_tv_ip() {
        let auth_context =
            BscpylgtvAuthContext::new().with_key_file_path("/tmp/lg-buddy/.aiopylgtv.sqlite");
        let client = BscpylgtvCommandClient::with_args(
            ip("192.168.1.42"),
            "/usr/bin/mock-bscpylgtv",
            ["--verbose"],
        )
        .with_auth_context(auth_context);

        let invocation = client.build_invocation("set_input", &[HdmiInput::Hdmi2.as_str()]);

        assert_eq!(invocation.program(), Path::new("/usr/bin/mock-bscpylgtv"));
        assert_eq!(
            invocation.args(),
            &[
                "--verbose".to_string(),
                "-p".to_string(),
                "/tmp/lg-buddy/.aiopylgtv.sqlite".to_string(),
                "192.168.1.42".to_string(),
                "set_input".to_string(),
                "HDMI_2".to_string(),
            ]
        );
        assert_eq!(invocation.launch_user(), None);
    }

    #[test]
    fn custom_launcher_receives_owner_user_and_key_file_path() {
        let launcher = RecordingLauncher::default();
        let auth_context = BscpylgtvAuthContext::new()
            .with_owner(SystemUser::new("vas", 1000, 1000, "/home/vas"))
            .with_key_file_path("/home/vas/.local/state/lg-buddy/.aiopylgtv.sqlite");
        let client = BscpylgtvCommandClient::new(ip("10.0.0.8"), "/usr/bin/mock-bscpylgtv")
            .with_auth_context(auth_context)
            .with_launcher(launcher.clone());

        client
            .unblank_screen()
            .expect("custom launcher should succeed");

        assert_eq!(
            launcher.calls(),
            vec![BscpylgtvInvocation::new(
                "/usr/bin/mock-bscpylgtv",
                vec![
                    "-p".to_string(),
                    "/home/vas/.local/state/lg-buddy/.aiopylgtv.sqlite".to_string(),
                    "10.0.0.8".to_string(),
                    "turn_screen_on".to_string(),
                ],
                Some(SystemUser::new("vas", 1000, 1000, "/home/vas")),
            )]
        );
    }

    #[test]
    fn command_timeout_is_attached_to_launcher_invocation() {
        let launcher = RecordingLauncher::default();
        let client = BscpylgtvCommandClient::new(ip("10.0.0.8"), "/usr/bin/mock-bscpylgtv")
            .with_command_timeout(Duration::from_secs(3))
            .with_launcher(launcher.clone());

        client.power_off().expect("custom launcher should succeed");

        let calls = launcher.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].timeout(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn direct_launcher_rejects_user_switch_requests_without_an_explicit_launcher() {
        let client = BscpylgtvCommandClient::new(ip("10.0.0.8"), "/usr/bin/mock-bscpylgtv")
            .with_auth_context(BscpylgtvAuthContext::new().with_owner(SystemUser::new(
                "vas",
                1000,
                1000,
                "/home/vas",
            )));
        let err = client
            .unblank_screen()
            .expect_err("direct launcher should reject owner-user requests");

        assert_eq!(err.operation(), TvOperation::UnblankScreen);
        assert_eq!(err.kind(), TvErrorKind::Transport);
        assert!(err
            .detail()
            .contains("cannot run `bscpylgtvcommand` as user `vas`"));
    }

    #[test]
    fn user_scoped_launch_plan_uses_current_identity_when_uid_matches() {
        let identity = SystemUser::new("vas", 1000, 1000, "/home/vas");

        let plan =
            build_user_scoped_launch_plan(1000, &identity).expect("matching uid should pass");

        assert_eq!(plan, UserScopedLaunchPlan::UseCurrentIdentity);
    }

    #[test]
    fn user_scoped_launch_plan_requires_privilege_drop_from_root_for_different_uid() {
        let identity = SystemUser::new("vas", 1000, 1000, "/home/vas");

        let plan =
            build_user_scoped_launch_plan(0, &identity).expect("root should be able to drop uid");

        assert_eq!(
            plan,
            UserScopedLaunchPlan::DropPrivileges {
                username: "vas".to_string(),
                uid: 1000,
                gid: 1000,
            }
        );
    }

    #[test]
    fn user_scoped_launch_plan_rejects_cross_user_request_from_non_root() {
        let identity = SystemUser::new("vas", 1000, 1000, "/home/vas");
        let err = build_user_scoped_launch_plan(1001, &identity)
            .expect_err("non-root cross-user launch should fail");

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string()
                .contains("cannot run `bscpylgtvcommand` as user `vas` from uid `1001`"),
            "io error was: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_scoped_launcher_sets_identity_environment_when_uid_already_matches() {
        let script = ExecutableScript::new(
            "tv-user-scoped-launcher-env",
            "print-env",
            "#!/bin/sh\nprintf 'USER=%s\\n' \"$USER\"\nprintf 'LOGNAME=%s\\n' \"$LOGNAME\"\nprintf 'HOME=%s\\n' \"$HOME\"\nid -u\n",
        );
        let current_uid = current_euid();
        let current_gid = unsafe { libc::getegid() };
        let identity = SystemUser::new(
            "lg-buddy-owner",
            current_uid,
            current_gid,
            "/tmp/lg-buddy-owner-home",
        );
        let invocation =
            BscpylgtvInvocation::new(script.path(), Vec::<String>::new(), Some(identity));

        let result = UserScopedBscpylgtvCommandLauncher
            .run(&invocation)
            .expect("same-uid launcher invocation should succeed");

        assert!(result.is_success(), "result was: {:?}", result);
        assert_eq!(
            result.stdout().lines().collect::<Vec<_>>(),
            vec![
                "USER=lg-buddy-owner",
                "LOGNAME=lg-buddy-owner",
                "HOME=/tmp/lg-buddy-owner-home",
                current_uid.to_string().as_str(),
            ]
        );
        assert_eq!(result.stderr(), "");
    }

    #[cfg(unix)]
    #[test]
    fn user_scoped_launcher_rejects_cross_user_request_without_root() {
        let script = ExecutableScript::new(
            "tv-user-scoped-launcher-denied",
            "should-not-run",
            "#!/bin/sh\nprintf 'unexpected\\n'\n",
        );
        let current_uid = current_euid();
        let current_gid = unsafe { libc::getegid() };
        let identity = SystemUser::new(
            "other-user",
            current_uid.saturating_add(1),
            current_gid,
            "/tmp/other-user-home",
        );
        let invocation =
            BscpylgtvInvocation::new(script.path(), Vec::<String>::new(), Some(identity));
        let err = UserScopedBscpylgtvCommandLauncher
            .run(&invocation)
            .expect_err("non-root cross-user launch should fail before exec");

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            err.to_string()
                .contains("cannot run `bscpylgtvcommand` as user `other-user`"),
            "io error was: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_scoped_launcher_drops_privileges_to_sudo_user_when_running_as_root() {
        if current_euid() != 0 {
            return;
        }

        let sudo_user = match std::env::var("SUDO_USER") {
            Ok(user) if !user.is_empty() && user != "root" => user,
            _ => return,
        };
        let identity =
            lookup_system_user_from_passwd(&sudo_user).expect("resolve sudo user from /etc/passwd");
        let script = ExecutableScript::new(
            "tv-user-scoped-launcher-root-drop",
            "print-identity",
            "#!/bin/sh\nprintf 'USER=%s\\n' \"$USER\"\nprintf 'LOGNAME=%s\\n' \"$LOGNAME\"\nprintf 'HOME=%s\\n' \"$HOME\"\nprintf 'UID=%s\\n' \"$(id -u)\"\nprintf 'GID=%s\\n' \"$(id -g)\"\nprintf 'GROUPS=%s\\n' \"$(id -G)\"\n",
        );
        let invocation =
            BscpylgtvInvocation::new(script.path(), Vec::<String>::new(), Some(identity.clone()));

        let result = UserScopedBscpylgtvCommandLauncher
            .run(&invocation)
            .expect("root should be able to drop privileges to the sudo user");

        assert!(result.is_success(), "result was: {:?}", result);
        let lines = result
            .stdout()
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            lines.first(),
            Some(&format!("USER={}", identity.username()))
        );
        assert_eq!(
            lines.get(1),
            Some(&format!("LOGNAME={}", identity.username()))
        );
        assert_eq!(
            lines.get(2),
            Some(&format!("HOME={}", identity.home().display()))
        );
        assert_eq!(lines.get(3), Some(&format!("UID={}", identity.uid())));
        assert_eq!(lines.get(4), Some(&format!("GID={}", identity.gid())));
        let groups_line = lines
            .get(5)
            .and_then(|line| line.strip_prefix("GROUPS="))
            .expect("groups line");
        let group_ids = groups_line.split_whitespace().collect::<Vec<_>>();
        assert!(
            group_ids
                .iter()
                .any(|group_id| *group_id == identity.gid().to_string()),
            "expected gid {} in groups {group_ids:?}",
            identity.gid()
        );
        if identity.gid() != 0 {
            assert!(
                group_ids.iter().all(|group_id| *group_id != "0"),
                "expected root group to be absent after initgroups, got {group_ids:?}"
            );
        }
        assert_eq!(result.stderr(), "");
    }

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("parse IPv4 address")
    }

    fn brightness(value: u8) -> OledBrightness {
        OledBrightness::new(value).expect("test brightness should be valid")
    }

    fn parse_mac(value: &str) -> MacAddress {
        value.parse().expect("parse mac address")
    }

    fn client_for_mock(mock: &MockBscpylgtv, tv_ip: Ipv4Addr) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(tv_ip, mock.command_path(), mock.command_args())
    }

    #[derive(Default)]
    struct RecordingWakeOnLanSender {
        calls: RefCell<Vec<(MacAddress, Option<Ipv4Addr>)>>,
    }

    impl RecordingWakeOnLanSender {
        fn calls(&self) -> Vec<(MacAddress, Option<Ipv4Addr>)> {
            self.calls.borrow().clone()
        }
    }

    impl WakeOnLanSender for RecordingWakeOnLanSender {
        fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError> {
            self.calls.borrow_mut().push((*mac, None));
            Ok(())
        }

        fn send_magic_packet_to(
            &self,
            mac: &MacAddress,
            target_ip: Ipv4Addr,
        ) -> Result<(), WakeOnLanError> {
            self.calls.borrow_mut().push((*mac, Some(target_ip)));
            Ok(())
        }
    }

    #[derive(Debug, Clone, Default)]
    struct RecordingLauncher {
        calls: Rc<RefCell<Vec<BscpylgtvInvocation>>>,
    }

    impl RecordingLauncher {
        fn calls(&self) -> Vec<BscpylgtvInvocation> {
            self.calls.borrow().clone()
        }
    }

    impl BscpylgtvCommandLauncher for RecordingLauncher {
        fn run(&self, invocation: &BscpylgtvInvocation) -> io::Result<BscpylgtvLaunchResult> {
            self.calls.borrow_mut().push(invocation.clone());
            Ok(BscpylgtvLaunchResult::new(
                Some(0),
                "{'returnValue': True}\n".to_string(),
                String::new(),
            ))
        }
    }

    #[cfg(unix)]
    fn lookup_system_user_from_passwd(username: &str) -> Option<SystemUser> {
        std::fs::read_to_string("/etc/passwd")
            .ok()?
            .lines()
            .find_map(|line| {
                let mut fields = line.split(':');
                let entry_username = fields.next()?;
                let _password = fields.next()?;
                let uid = fields.next()?.parse::<u32>().ok()?;
                let gid = fields.next()?.parse::<u32>().ok()?;
                let _gecos = fields.next()?;
                let home = fields.next()?;
                let _shell = fields.next()?;

                (entry_username == username)
                    .then(|| SystemUser::new(entry_username, uid, gid, home))
            })
    }
}
