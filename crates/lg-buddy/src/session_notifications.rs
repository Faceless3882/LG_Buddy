use std::collections::{HashMap, VecDeque};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
use dbus::blocking::Connection as DbusConnection;
use dbus::channel::MatchingReceiver;
use dbus::message::{MatchRule as DbusMatchRule, MessageType as DbusMessageType};
use dbus_crossroads::{Crossroads, MethodErr};
use semver::Version;

use crate::notifications::{
    parse_notification_signal, FreedesktopNotifier, Notification, NotificationAction,
    NotificationError, NotificationId, NotificationSignal, Notifier, NOTIFICATION_INTERFACE,
    NOTIFICATION_PATH, NOTIFICATION_SERVICE,
};
use crate::session_bus::{
    bus_signal_from_dbus_message, new_session_bus_client, BusMethodCall, BusValue,
    SessionBusClient, SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};
use crate::settings::{
    run_settings_command, ConfigPathResolver, SettingsCommand, SettingsCommandRunner,
    SettingsError, SettingsStore,
};
use crate::updates::UpdateChannel;
use crate::version::ReleaseChannel;

pub(crate) const SESSION_BUS_NAME: &str = "io.github.Staphylococcus.LGBuddy";
pub(crate) const SESSION_OBJECT_PATH: &str = "/io/github/Staphylococcus/LGBuddy/Session";
pub(crate) const SESSION_INTERFACE: &str = "io.github.Staphylococcus.LGBuddy.Session1";
pub(crate) const SHOW_UPDATE_NOTIFICATION_METHOD: &str = "ShowUpdateNotification";
pub(crate) const VIEW_RELEASE_ACTION_KEY: &str = "view-release";
pub(crate) const DISABLE_UPDATE_CHECKS_ACTION_KEY: &str = "disable-update-checks";

const SESSION_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(2);
const NOTIFICATION_OWNER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);
const RECENTLY_CLOSED_NOTIFICATION_LIMIT: usize = 16;
const GNOME_SHELL_BUS_NAME: &str = "org.gnome.Shell";
const GNOME_SHELL_PROCESS_NAME: &str = "gnome-shell";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateNotificationRequest {
    check_channel: UpdateChannel,
    current_version: Version,
    current_channel: ReleaseChannel,
    latest_version: Version,
    latest_channel: UpdateChannel,
    release_url: String,
}

impl UpdateNotificationRequest {
    pub(crate) fn new(
        check_channel: UpdateChannel,
        current_version: Version,
        current_channel: ReleaseChannel,
        latest_version: Version,
        latest_channel: UpdateChannel,
        release_url: impl Into<String>,
    ) -> Result<Self, UpdateNotificationError> {
        let release_url = release_url.into();
        if !valid_release_url(&release_url) {
            return Err(UpdateNotificationError::InvalidRequest(format!(
                "invalid release URL `{release_url}`"
            )));
        }

        Ok(Self {
            check_channel,
            current_version,
            current_channel,
            latest_version,
            latest_channel,
            release_url,
        })
    }

    pub(crate) fn from_dbus_fields(
        check_channel: String,
        current_version: String,
        current_channel: String,
        latest_version: String,
        latest_channel: String,
        release_url: String,
    ) -> Result<Self, UpdateNotificationError> {
        Self::new(
            parse_update_channel(&check_channel)?,
            parse_version("current version", &current_version)?,
            parse_release_channel(&current_channel)?,
            parse_version("latest version", &latest_version)?,
            parse_update_channel(&latest_channel)?,
            release_url,
        )
    }

    pub(crate) fn to_dbus_fields(&self) -> (String, String, String, String, String, String) {
        (
            self.check_channel.as_str().to_string(),
            self.current_version.to_string(),
            self.current_channel.as_str().to_string(),
            self.latest_version.to_string(),
            self.latest_channel.as_str().to_string(),
            self.release_url.clone(),
        )
    }

    fn to_bus_body(&self) -> Vec<BusValue> {
        let (check_channel, current_version, current_channel, latest_version, latest_channel, url) =
            self.to_dbus_fields();
        vec![
            BusValue::String(check_channel),
            BusValue::String(current_version),
            BusValue::String(current_channel),
            BusValue::String(latest_version),
            BusValue::String(latest_channel),
            BusValue::String(url),
        ]
    }

    fn from_bus_body(body: &[BusValue]) -> Result<Self, UpdateNotificationError> {
        let [BusValue::String(check_channel), BusValue::String(current_version), BusValue::String(current_channel), BusValue::String(latest_version), BusValue::String(latest_channel), BusValue::String(release_url)] =
            body
        else {
            return Err(UpdateNotificationError::InvalidRequest(
                "expected D-Bus update notification body with 6 string fields".to_string(),
            ));
        };

        Self::from_dbus_fields(
            check_channel.clone(),
            current_version.clone(),
            current_channel.clone(),
            latest_version.clone(),
            latest_channel.clone(),
            release_url.clone(),
        )
    }

    pub(crate) fn release_url(&self) -> &str {
        &self.release_url
    }

    fn notification(&self, actions_supported: bool) -> Notification {
        let mut notification = Notification::new(
            "LG Buddy update available",
            format!(
                "LG Buddy {} ({}) is available.\nCurrent: {} ({})\nInstall: lg-buddy updates install\n{}",
                self.latest_version,
                self.latest_channel.as_str(),
                self.current_version,
                self.current_channel.as_str(),
                self.release_url
            ),
        );

        if actions_supported {
            notification.actions = vec![
                NotificationAction {
                    key: DISABLE_UPDATE_CHECKS_ACTION_KEY.to_string(),
                    label: "Never Notify Again".to_string(),
                },
                NotificationAction {
                    key: VIEW_RELEASE_ACTION_KEY.to_string(),
                    label: "View Release".to_string(),
                },
            ];
        }

        notification
    }
}

fn valid_release_url(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
}

fn parse_update_channel(value: &str) -> Result<UpdateChannel, UpdateNotificationError> {
    match value {
        "stable" => Ok(UpdateChannel::Stable),
        "prerelease" => Ok(UpdateChannel::Prerelease),
        other => Err(UpdateNotificationError::InvalidRequest(format!(
            "invalid update channel `{other}`"
        ))),
    }
}

fn parse_release_channel(value: &str) -> Result<ReleaseChannel, UpdateNotificationError> {
    match value {
        "dev" => Ok(ReleaseChannel::Dev),
        "prerelease" => Ok(ReleaseChannel::Prerelease),
        "stable" => Ok(ReleaseChannel::Stable),
        other => Err(UpdateNotificationError::InvalidRequest(format!(
            "invalid release channel `{other}`"
        ))),
    }
}

fn parse_version(label: &'static str, value: &str) -> Result<Version, UpdateNotificationError> {
    Version::parse(value).map_err(|err| {
        UpdateNotificationError::InvalidRequest(format!("invalid {label} `{value}`: {err}"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateNotificationOutcome {
    Sent,
}

impl UpdateNotificationOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
        }
    }

    fn parse(value: &str) -> Result<Self, UpdateNotificationError> {
        match value {
            "sent" => Ok(Self::Sent),
            other => Err(UpdateNotificationError::Transport(format!(
                "LG Buddy session service returned unknown update notification outcome `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateNotificationError {
    InvalidRequest(String),
    Transport(String),
    Notification(NotificationError),
    Session(SessionServiceError),
    Settings(SettingsError),
    OpenRelease { url: String, message: String },
}

impl fmt::Display for UpdateNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => {
                write!(f, "invalid update notification request: {message}")
            }
            Self::Transport(message) => {
                write!(f, "could not request update notification from LG Buddy session service: {message}")
            }
            Self::Notification(err) => write!(f, "{err}"),
            Self::Session(err) => write!(f, "{err}"),
            Self::Settings(err) => {
                write!(f, "could not disable automatic update checks: {err}")
            }
            Self::OpenRelease { url, message } => {
                write!(f, "could not open release URL `{url}`: {message}")
            }
        }
    }
}

impl Error for UpdateNotificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Notification(err) => Some(err),
            Self::Session(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::InvalidRequest(_) | Self::Transport(_) | Self::OpenRelease { .. } => None,
        }
    }
}

impl From<NotificationError> for UpdateNotificationError {
    fn from(value: NotificationError) -> Self {
        Self::Notification(value)
    }
}

impl From<SessionServiceError> for UpdateNotificationError {
    fn from(value: SessionServiceError) -> Self {
        Self::Session(value)
    }
}

impl From<SettingsError> for UpdateNotificationError {
    fn from(value: SettingsError) -> Self {
        Self::Settings(value)
    }
}

pub(crate) trait UpdateNotificationHandoff {
    fn show_update_notification(
        &self,
        request: &UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SessionBusUpdateNotificationHandoff;

impl UpdateNotificationHandoff for SessionBusUpdateNotificationHandoff {
    fn show_update_notification(
        &self,
        request: &UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
        let mut bus = new_session_bus_client()
            .map_err(|err| UpdateNotificationError::Transport(err.to_string()))?;
        show_update_notification_over_session_bus(bus.as_mut(), request)
    }
}

fn show_update_notification_over_session_bus<C: SessionBusClient + ?Sized>(
    bus: &mut C,
    request: &UpdateNotificationRequest,
) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
    let reply = bus
        .call_method(
            BusMethodCall::new(
                SESSION_BUS_NAME,
                SESSION_OBJECT_PATH,
                SESSION_INTERFACE,
                SHOW_UPDATE_NOTIFICATION_METHOD,
            )
            .with_body(request.to_bus_body()),
        )
        .map_err(|err| UpdateNotificationError::Transport(err.to_string()))?;
    let outcome = reply
        .single_string()
        .map_err(|err| UpdateNotificationError::Transport(err.to_string()))?;

    UpdateNotificationOutcome::parse(outcome)
}

pub(crate) trait ReleaseOpener {
    fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError>;
}

pub(crate) trait UpdateNotificationPreferences {
    fn disable_automatic_update_checks(&self) -> Result<(), UpdateNotificationError>;
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SettingsUpdateNotificationPreferences {
    config_path: Option<PathBuf>,
}

impl SettingsUpdateNotificationPreferences {
    fn from_env() -> Self {
        Self {
            config_path: ConfigPathResolver::resolve_from_env().ok(),
        }
    }
}

impl UpdateNotificationPreferences for SettingsUpdateNotificationPreferences {
    fn disable_automatic_update_checks(&self) -> Result<(), UpdateNotificationError> {
        let command = SettingsCommand::Set {
            key: "updates.auto_check".to_string(),
            value: "disabled".to_string(),
        };
        let mut output = Vec::new();
        if let Some(config_path) = &self.config_path {
            let store = SettingsStore::load(config_path)?;
            SettingsCommandRunner::new(store).run(command, &mut output)
        } else {
            run_settings_command(command, &mut output)
        }
        .map_err(UpdateNotificationError::Settings)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SystemReleaseOpener {
    command_path: PathBuf,
}

impl Default for SystemReleaseOpener {
    fn default() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_XDG_OPEN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("xdg-open")),
        }
    }
}

impl ReleaseOpener for SystemReleaseOpener {
    fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError> {
        let output = ProcessCommand::new(&self.command_path)
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| UpdateNotificationError::OpenRelease {
                url: url.to_string(),
                message: err.to_string(),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(UpdateNotificationError::OpenRelease {
                url: url.to_string(),
                message: if stderr.is_empty() {
                    format!("opener exited with status {}", output.status)
                } else {
                    stderr
                },
            })
        }
    }
}

pub(crate) struct SessionUpdateNotificationDispatcher<N, O, P> {
    notifier: N,
    opener: O,
    preferences: P,
    pending: HashMap<NotificationId, UpdateNotificationRequest>,
    recently_closed: VecDeque<(NotificationId, UpdateNotificationRequest)>,
}

impl<N, O, P> SessionUpdateNotificationDispatcher<N, O, P> {
    fn new(notifier: N, opener: O, preferences: P) -> Self {
        Self {
            notifier,
            opener,
            preferences,
            pending: HashMap::new(),
            recently_closed: VecDeque::new(),
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    fn recently_closed_len(&self) -> usize {
        self.recently_closed.len()
    }
}

impl<N, O, P> SessionUpdateNotificationDispatcher<N, O, P>
where
    N: Notifier,
    O: ReleaseOpener,
    P: UpdateNotificationPreferences,
{
    fn show_update_notification(
        &mut self,
        request: UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
        let capabilities = self.notifier.capabilities()?;
        let notification = request.notification(capabilities.actions);
        let notification_id = self.notifier.notify(&notification)?;

        if capabilities.actions {
            self.pending.insert(notification_id, request);
        }

        Ok(UpdateNotificationOutcome::Sent)
    }

    fn handle_notification_signal(
        &mut self,
        signal: NotificationSignal,
    ) -> Result<Option<SessionNotificationEvent>, UpdateNotificationError> {
        match signal {
            NotificationSignal::ActionInvoked { id, action_key } => {
                let Some(request) = self.take_action_request(id) else {
                    return Ok(None);
                };

                match action_key.as_str() {
                    VIEW_RELEASE_ACTION_KEY => {
                        self.opener.open_release(request.release_url())?;
                        Ok(Some(SessionNotificationEvent::ReleaseOpened))
                    }
                    DISABLE_UPDATE_CHECKS_ACTION_KEY => {
                        self.preferences.disable_automatic_update_checks()?;
                        Ok(Some(SessionNotificationEvent::UpdateChecksDisabled))
                    }
                    _ => Ok(Some(SessionNotificationEvent::UnknownAction)),
                }
            }
            NotificationSignal::Closed { id, .. } => {
                if let Some(request) = self.pending.remove(&id) {
                    self.remember_recently_closed(id, request);
                    Ok(Some(SessionNotificationEvent::Closed))
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn take_action_request(&mut self, id: NotificationId) -> Option<UpdateNotificationRequest> {
        self.pending.remove(&id).or_else(|| {
            let index = self
                .recently_closed
                .iter()
                .position(|(closed_id, _)| *closed_id == id)?;
            self.recently_closed
                .remove(index)
                .map(|(_, request)| request)
        })
    }

    fn remember_recently_closed(&mut self, id: NotificationId, request: UpdateNotificationRequest) {
        if self.recently_closed.len() == RECENTLY_CLOSED_NOTIFICATION_LIMIT {
            self.recently_closed.pop_front();
        }
        self.recently_closed.push_back((id, request));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionNotificationEvent {
    ReleaseOpened,
    UpdateChecksDisabled,
    Closed,
    UnknownAction,
}

impl SessionNotificationEvent {
    fn as_log_message(self) -> &'static str {
        match self {
            Self::ReleaseOpened => "release URL opened from notification action",
            Self::UpdateChecksDisabled => {
                "automatic update checks disabled from notification action"
            }
            Self::Closed => "update notification closed",
            Self::UnknownAction => "unknown notification action ignored",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionServiceError {
    Transport(String),
    NameUnavailable {
        name: &'static str,
        reply: RequestNameReply,
    },
    StartupTimeout,
    Poisoned,
}

impl fmt::Display for SessionServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => {
                write!(f, "LG Buddy session D-Bus service error: {message}")
            }
            Self::NameUnavailable { name, reply } => {
                write!(
                    f,
                    "could not own LG Buddy session D-Bus name `{name}`: {reply:?}"
                )
            }
            Self::StartupTimeout => write!(f, "timed out starting LG Buddy session D-Bus service"),
            Self::Poisoned => write!(f, "LG Buddy session notification state lock was poisoned"),
        }
    }
}

impl Error for SessionServiceError {}

impl From<SessionBusError> for SessionServiceError {
    fn from(value: SessionBusError) -> Self {
        Self::Transport(value.to_string())
    }
}

pub(crate) struct SessionNotificationServiceThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for SessionNotificationServiceThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn spawn_session_notification_service(
) -> Result<SessionNotificationServiceThread, SessionServiceError> {
    let dispatcher = SessionUpdateNotificationDispatcher::new(
        FreedesktopNotifier,
        SystemReleaseOpener::default(),
        SettingsUpdateNotificationPreferences::from_env(),
    );
    spawn_session_notification_service_with(dispatcher)
}

fn spawn_session_notification_service_with<N, O, P>(
    dispatcher: SessionUpdateNotificationDispatcher<N, O, P>,
) -> Result<SessionNotificationServiceThread, SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
    P: UpdateNotificationPreferences + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let started = Arc::new(AtomicBool::new(false));
    let thread_started = Arc::clone(&started);
    let (ready_sender, ready_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let loop_ready = ready_sender.clone();
        let result = run_session_notification_service_loop(
            dispatcher,
            thread_stop,
            loop_ready,
            thread_started,
        );
        if let Err(err) = result {
            if started.load(Ordering::SeqCst) {
                eprintln!("LG Buddy Session: {err}");
            } else {
                let _ = ready_sender.send(Err(err));
            }
        }
    });

    wait_for_session_notification_service_start_with_timeout(
        stop,
        handle,
        ready_receiver,
        SESSION_START_TIMEOUT,
    )
}

fn wait_for_session_notification_service_start_with_timeout(
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    ready_receiver: mpsc::Receiver<Result<(), SessionServiceError>>,
    timeout: Duration,
) -> Result<SessionNotificationServiceThread, SessionServiceError> {
    match ready_receiver.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(SessionNotificationServiceThread {
            stop,
            handle: Some(handle),
        }),
        Ok(Err(err)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err(err)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::SeqCst);
            drop(handle);
            Err(SessionServiceError::StartupTimeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err(SessionServiceError::Transport(
                "session service thread exited before startup completed".to_string(),
            ))
        }
    }
}

fn run_session_notification_service_loop<N, O, P>(
    dispatcher: SessionUpdateNotificationDispatcher<N, O, P>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), SessionServiceError>>,
    started: Arc<AtomicBool>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
    P: UpdateNotificationPreferences + Send + 'static,
{
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    let connection = DbusConnection::new_session()
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    let name_reply = connection
        .request_name(SESSION_BUS_NAME, false, false, true)
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    match name_reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {}
        reply => {
            return Err(SessionServiceError::NameUnavailable {
                name: SESSION_BUS_NAME,
                reply,
            })
        }
    }

    let dispatcher = Arc::new(Mutex::new(dispatcher));
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    register_session_methods(&connection, Arc::clone(&dispatcher))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    register_notification_signal_handler(&connection, dispatcher)?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    if ready.send(Ok(())).is_err() {
        return Ok(());
    }
    started.store(true, Ordering::SeqCst);

    while !stop.load(Ordering::SeqCst) {
        connection
            .process(SESSION_PROCESS_INTERVAL)
            .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    }

    Ok(())
}

fn session_service_startup_stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::SeqCst)
}

fn register_session_methods<N, O, P>(
    connection: &DbusConnection,
    dispatcher: Arc<Mutex<SessionUpdateNotificationDispatcher<N, O, P>>>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
    P: UpdateNotificationPreferences + Send + 'static,
{
    let mut crossroads = Crossroads::new();
    let method_dispatcher = Arc::clone(&dispatcher);
    let iface = crossroads.register(SESSION_INTERFACE, move |builder| {
        let method_dispatcher = Arc::clone(&method_dispatcher);
        builder.method(
            SHOW_UPDATE_NOTIFICATION_METHOD,
            (
                "check_channel",
                "current_version",
                "current_channel",
                "latest_version",
                "latest_channel",
                "release_url",
            ),
            ("outcome",),
            move |_,
                  _,
                  (
                check_channel,
                current_version,
                current_channel,
                latest_version,
                latest_channel,
                release_url,
            ): (String, String, String, String, String, String)| {
                let body = vec![
                    BusValue::String(check_channel),
                    BusValue::String(current_version),
                    BusValue::String(current_channel),
                    BusValue::String(latest_version),
                    BusValue::String(latest_channel),
                    BusValue::String(release_url),
                ];
                let request = UpdateNotificationRequest::from_bus_body(&body)
                    .map_err(|err| MethodErr::failed(&err.to_string()))?;

                let outcome = method_dispatcher
                    .lock()
                    .map_err(|_| MethodErr::failed(&SessionServiceError::Poisoned.to_string()))?
                    .show_update_notification(request)
                    .map_err(|err| MethodErr::failed(&err.to_string()))?;

                Ok((outcome.as_str().to_string(),))
            },
        );
    });
    crossroads.insert(SESSION_OBJECT_PATH, &[iface], ());

    let shared_crossroads = Arc::new(Mutex::new(crossroads));
    let crossroads_receiver = Arc::clone(&shared_crossroads);
    connection.start_receive(
        DbusMatchRule::new_method_call(),
        Box::new(move |message, conn| {
            let result = crossroads_receiver
                .lock()
                .map_err(|_| SessionServiceError::Poisoned)
                .and_then(|mut crossroads| {
                    crossroads
                        .handle_message(message, conn)
                        .map(|_| ())
                        .map_err(|_| {
                            SessionServiceError::Transport(
                                "failed to handle session D-Bus method call".to_string(),
                            )
                        })
                });

            if let Err(err) = result {
                eprintln!("LG Buddy Session: {err}");
            }
            true
        }),
    );

    Ok(())
}

fn register_notification_signal_handler<N, O, P>(
    connection: &DbusConnection,
    dispatcher: Arc<Mutex<SessionUpdateNotificationDispatcher<N, O, P>>>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
    P: UpdateNotificationPreferences + Send + 'static,
{
    let signal_rule = DbusMatchRule::new()
        .with_type(DbusMessageType::Signal)
        .with_path(NOTIFICATION_PATH)
        .with_interface(NOTIFICATION_INTERFACE);
    connection
        .add_match_no_cb(&signal_rule.match_str())
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;

    connection.start_receive(
        signal_rule,
        Box::new(move |message, conn| {
            let bus_signal = match bus_signal_from_dbus_message(message) {
                Ok(signal) => signal,
                Err(err) => {
                    eprintln!("LG Buddy Session: notification signal parse failed: {err}");
                    return true;
                }
            };
            let Some(notification_signal) = parse_notification_signal(&bus_signal) else {
                eprintln!(
                    "LG Buddy Session: ignored unsupported notification signal `{}` from `{}`",
                    bus_signal.member,
                    bus_signal.sender.as_deref().unwrap_or("<missing sender>")
                );
                return true;
            };
            let signal_description = describe_notification_signal(&notification_signal);

            let trusted_signal_senders = match trusted_notification_signal_senders(conn) {
                Ok(owners) => owners,
                Err(err) => {
                    eprintln!("LG Buddy Session: notification signal owner check failed: {err}");
                    return true;
                }
            };
            if !notification_signal_sender_is_trusted(&bus_signal, &trusted_signal_senders) {
                eprintln!(
                    "LG Buddy Session: ignored {signal_description} from untrusted sender `{}`; trusted senders: {}",
                    bus_signal.sender.as_deref().unwrap_or("<missing sender>"),
                    trusted_signal_senders.join(", ")
                );
                return true;
            }

            let result = dispatcher
                .lock()
                .map_err(|_| UpdateNotificationError::Session(SessionServiceError::Poisoned))
                .and_then(|mut dispatcher| dispatcher.handle_notification_signal(notification_signal));

            match result {
                Ok(Some(event)) => {
                    eprintln!(
                        "LG Buddy Session: handled {signal_description}: {}",
                        event.as_log_message()
                    );
                }
                Ok(None) => {
                    eprintln!(
                        "LG Buddy Session: ignored {signal_description}: no pending update notification"
                    );
                }
                Err(err) => {
                    eprintln!("LG Buddy Session: update notification action failed: {err}");
                }
            }

            true
        }),
    );

    Ok(())
}

fn trusted_notification_signal_senders(
    connection: &DbusConnection,
) -> Result<Vec<String>, SessionServiceError> {
    let notification_owner = current_bus_name_owner(connection, NOTIFICATION_SERVICE)?;
    let gnome_shell_owner = match current_bus_name_owner(connection, GNOME_SHELL_BUS_NAME) {
        Ok(owner) => match current_bus_name_owner_process_identity(connection, &owner) {
            Ok(identity) => Some((owner, identity)),
            Err(err) => {
                eprintln!(
                    "LG Buddy Session: could not verify `{GNOME_SHELL_BUS_NAME}` owner as gnome-shell: {err}"
                );
                None
            }
        },
        Err(_) => None,
    };

    Ok(trusted_notification_signal_senders_from_candidates(
        notification_owner,
        gnome_shell_owner,
    ))
}

fn trusted_notification_signal_senders_from_candidates(
    notification_owner: String,
    gnome_shell_owner: Option<(String, BusProcessIdentity)>,
) -> Vec<String> {
    let mut owners = vec![notification_owner];
    if let Some((owner, identity)) = gnome_shell_owner {
        if !owners.contains(&owner) && process_identity_is_gnome_shell(&identity) {
            owners.push(owner);
        }
    }
    owners
}

fn current_bus_name_owner(
    connection: &DbusConnection,
    name: &str,
) -> Result<String, SessionServiceError> {
    let proxy = connection.with_proxy(
        DBUS_SERVICE_NAME,
        DBUS_OBJECT_PATH,
        NOTIFICATION_OWNER_LOOKUP_TIMEOUT,
    );
    let (owner,): (String,) = proxy
        .method_call(DBUS_INTERFACE, "GetNameOwner", (name,))
        .map_err(|err| {
            SessionServiceError::Transport(format!("could not resolve `{name}` owner: {err}"))
        })?;

    Ok(owner)
}

fn current_bus_name_owner_process_identity(
    connection: &DbusConnection,
    owner: &str,
) -> Result<BusProcessIdentity, SessionServiceError> {
    let proxy = connection.with_proxy(
        DBUS_SERVICE_NAME,
        DBUS_OBJECT_PATH,
        NOTIFICATION_OWNER_LOOKUP_TIMEOUT,
    );
    let (pid,): (u32,) = proxy
        .method_call(DBUS_INTERFACE, "GetConnectionUnixProcessID", (owner,))
        .map_err(|err| {
            SessionServiceError::Transport(format!("could not resolve `{owner}` process id: {err}"))
        })?;

    Ok(read_bus_process_identity(pid))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BusProcessIdentity {
    comm: Option<String>,
    exe_name: Option<String>,
}

fn read_bus_process_identity(pid: u32) -> BusProcessIdentity {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let exe_name = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|value| !value.is_empty());

    BusProcessIdentity { comm, exe_name }
}

fn process_identity_is_gnome_shell(identity: &BusProcessIdentity) -> bool {
    identity.comm.as_deref() == Some(GNOME_SHELL_PROCESS_NAME)
        && identity.exe_name.as_deref() == Some(GNOME_SHELL_PROCESS_NAME)
}

fn notification_signal_sender_is_trusted(
    signal: &crate::session_bus::BusSignal,
    trusted_senders: &[String],
) -> bool {
    signal
        .sender
        .as_ref()
        .is_some_and(|sender| trusted_senders.contains(sender))
}

fn describe_notification_signal(signal: &NotificationSignal) -> String {
    match signal {
        NotificationSignal::ActionInvoked { id, action_key } => {
            format!(
                "notification action `{action_key}` for notification {}",
                id.0
            )
        }
        NotificationSignal::Closed { id, reason } => {
            format!("notification close for notification {} ({reason:?})", id.0)
        }
    }
}

#[cfg(test)]
fn handle_notification_bus_signal<N, O, P>(
    dispatcher: &mut SessionUpdateNotificationDispatcher<N, O, P>,
    bus_signal: &crate::session_bus::BusSignal,
    trusted_signal_senders: &[String],
) -> Result<Option<SessionNotificationEvent>, UpdateNotificationError>
where
    N: Notifier,
    O: ReleaseOpener,
    P: UpdateNotificationPreferences,
{
    let Some(notification_signal) = parse_notification_signal(bus_signal) else {
        return Ok(None);
    };

    if !notification_signal_sender_is_trusted(bus_signal, trusted_signal_senders) {
        return Ok(None);
    }

    dispatcher.handle_notification_signal(notification_signal)
}

#[cfg(test)]
mod tests {
    use super::{
        handle_notification_bus_signal, notification_signal_sender_is_trusted,
        process_identity_is_gnome_shell, show_update_notification_over_session_bus,
        trusted_notification_signal_senders_from_candidates,
        wait_for_session_notification_service_start_with_timeout, BusProcessIdentity,
        ReleaseOpener, SessionNotificationEvent, SessionServiceError,
        SessionUpdateNotificationDispatcher, UpdateNotificationError, UpdateNotificationOutcome,
        UpdateNotificationPreferences, UpdateNotificationRequest, DISABLE_UPDATE_CHECKS_ACTION_KEY,
        SESSION_BUS_NAME, SESSION_INTERFACE, SESSION_OBJECT_PATH, SHOW_UPDATE_NOTIFICATION_METHOD,
        VIEW_RELEASE_ACTION_KEY,
    };
    use crate::notifications::{
        Notification, NotificationCapabilities, NotificationError, NotificationId,
        NotificationSignal, Notifier, NOTIFICATION_INTERFACE, NOTIFICATION_PATH,
    };
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use crate::updates::UpdateChannel;
    use crate::version::ReleaseChannel;
    use semver::Version;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Debug)]
    struct RecordingNotifier {
        capabilities: NotificationCapabilities,
        result: Result<NotificationId, NotificationError>,
        notifications: RefCell<Vec<Notification>>,
    }

    impl RecordingNotifier {
        fn new(actions: bool) -> Self {
            Self {
                capabilities: NotificationCapabilities { actions },
                result: Ok(NotificationId(7)),
                notifications: RefCell::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                capabilities: NotificationCapabilities { actions: true },
                result: Err(NotificationError::Transport("bus unavailable".to_string())),
                notifications: RefCell::new(Vec::new()),
            }
        }

        fn notifications(&self) -> Vec<Notification> {
            self.notifications.borrow().clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError> {
            Ok(self.capabilities)
        }

        fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
            self.notifications.borrow_mut().push(notification.clone());
            self.result.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedBusMethodCall {
        destination: String,
        path: String,
        interface: String,
        member: String,
        body: Vec<BusValue>,
    }

    impl From<BusMethodCall<'_>> for RecordedBusMethodCall {
        fn from(value: BusMethodCall<'_>) -> Self {
            Self {
                destination: value.destination.to_string(),
                path: value.path.to_string(),
                interface: value.interface.to_string(),
                member: value.member.to_string(),
                body: value.body,
            }
        }
    }

    #[derive(Debug)]
    struct RecordingSessionBus {
        calls: Vec<RecordedBusMethodCall>,
        replies: VecDeque<Result<BusReply, SessionBusError>>,
    }

    impl RecordingSessionBus {
        fn new(replies: Vec<Result<BusReply, SessionBusError>>) -> Self {
            Self {
                calls: Vec::new(),
                replies: replies.into(),
            }
        }
    }

    impl SessionBusClient for RecordingSessionBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            Ok(true)
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.calls.push(call.into());
            self.replies.pop_front().unwrap_or_else(|| {
                Err(SessionBusError::Transport(
                    "unexpected method call".to_string(),
                ))
            })
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            Ok(None)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingOpener {
        opened: RefCell<Vec<String>>,
        fail: bool,
    }

    impl RecordingOpener {
        fn failing() -> Self {
            Self {
                opened: RefCell::new(Vec::new()),
                fail: true,
            }
        }

        fn opened(&self) -> Vec<String> {
            self.opened.borrow().clone()
        }
    }

    impl ReleaseOpener for RecordingOpener {
        fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError> {
            self.opened.borrow_mut().push(url.to_string());
            if self.fail {
                Err(UpdateNotificationError::OpenRelease {
                    url: url.to_string(),
                    message: "open failed".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingPreferences {
        disable_calls: RefCell<usize>,
        fail: bool,
    }

    impl RecordingPreferences {
        fn failing() -> Self {
            Self {
                disable_calls: RefCell::new(0),
                fail: true,
            }
        }

        fn disable_calls(&self) -> usize {
            *self.disable_calls.borrow()
        }
    }

    impl UpdateNotificationPreferences for RecordingPreferences {
        fn disable_automatic_update_checks(&self) -> Result<(), UpdateNotificationError> {
            *self.disable_calls.borrow_mut() += 1;
            if self.fail {
                Err(UpdateNotificationError::Transport(
                    "settings update failed".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }

    fn request() -> UpdateNotificationRequest {
        UpdateNotificationRequest::new(
            UpdateChannel::Stable,
            Version::parse("1.1.0").unwrap(),
            ReleaseChannel::Stable,
            Version::parse("1.1.1").unwrap(),
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        )
        .unwrap()
    }

    #[test]
    fn request_round_trips_through_dbus_fields() {
        let request = request();
        let fields = request.to_dbus_fields();

        let parsed = UpdateNotificationRequest::from_dbus_fields(
            fields.0, fields.1, fields.2, fields.3, fields.4, fields.5,
        )
        .expect("request should parse");

        assert_eq!(parsed, request);
    }

    #[test]
    fn request_round_trips_through_bus_body() {
        let request = request();
        let body = request.to_bus_body();

        let parsed = UpdateNotificationRequest::from_bus_body(&body).expect("request should parse");

        assert_eq!(parsed, request);
    }

    #[test]
    fn handoff_sends_update_notification_method_call_over_session_bus() {
        let mut bus = RecordingSessionBus::new(vec![Ok(BusReply::new(vec![BusValue::String(
            "sent".to_string(),
        )]))]);

        let outcome = show_update_notification_over_session_bus(&mut bus, &request())
            .expect("handoff should succeed");

        assert_eq!(outcome, UpdateNotificationOutcome::Sent);
        assert_eq!(
            bus.calls,
            vec![RecordedBusMethodCall {
                destination: SESSION_BUS_NAME.to_string(),
                path: SESSION_OBJECT_PATH.to_string(),
                interface: SESSION_INTERFACE.to_string(),
                member: SHOW_UPDATE_NOTIFICATION_METHOD.to_string(),
                body: vec![
                    BusValue::String("stable".to_string()),
                    BusValue::String("1.1.0".to_string()),
                    BusValue::String("stable".to_string()),
                    BusValue::String("1.1.1".to_string()),
                    BusValue::String("stable".to_string()),
                    BusValue::String("https://github.test/releases/tag/v1.1.1".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn invalid_request_fields_are_rejected() {
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "nightly".to_string(),
            "1.1.0".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "https://github.test/releases/tag/v1.1.1".to_string(),
        )
        .is_err());
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "stable".to_string(),
            "not-semver".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "https://github.test/releases/tag/v1.1.1".to_string(),
        )
        .is_err());
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "stable".to_string(),
            "1.1.0".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "".to_string(),
        )
        .is_err());
        assert!(UpdateNotificationRequest::from_bus_body(&[BusValue::U32(7)]).is_err());
    }

    fn action_invoked_bus_signal(action_key: &str, sender: &str) -> BusSignal {
        BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked")
            .with_sender(sender)
            .with_body(vec![
                BusValue::U32(7),
                BusValue::String(action_key.to_string()),
            ])
    }

    fn notification_closed_bus_signal(sender: &str) -> BusSignal {
        BusSignal::new(
            NOTIFICATION_PATH,
            NOTIFICATION_INTERFACE,
            "NotificationClosed",
        )
        .with_sender(sender)
        .with_body(vec![BusValue::U32(7), BusValue::U32(2)])
    }

    #[test]
    fn action_capable_notification_attaches_view_release_button() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);

        let outcome = dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        assert_eq!(outcome, UpdateNotificationOutcome::Sent);
        let notifications = dispatcher.notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].actions.len(), 2);
        assert_eq!(
            notifications[0].actions[0].key,
            DISABLE_UPDATE_CHECKS_ACTION_KEY
        );
        assert_eq!(notifications[0].actions[0].label, "Never Notify Again");
        assert_eq!(notifications[0].actions[1].key, VIEW_RELEASE_ACTION_KEY);
        assert_eq!(notifications[0].actions[1].label, "View Release");
        assert!(notifications[0]
            .body
            .contains("Install: lg-buddy updates install"));
        assert!(notifications[0]
            .body
            .contains("https://github.test/releases/tag/v1.1.1"));
        assert_eq!(dispatcher.pending_len(), 1);
    }

    #[test]
    fn passive_notification_does_not_store_pending_action_state() {
        let notifier = RecordingNotifier::new(false);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);

        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        assert!(dispatcher.notifier.notifications()[0].actions.is_empty());
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn notification_failure_does_not_store_pending_state() {
        let notifier = RecordingNotifier::failing();
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);

        dispatcher
            .show_update_notification(request())
            .expect_err("notification should fail");

        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn startup_timeout_does_not_join_blocked_worker() {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ready_sender = ready_sender;
            let _ = release_receiver.recv_timeout(Duration::from_secs(5));
        });

        let started = Instant::now();
        let result = wait_for_session_notification_service_start_with_timeout(
            Arc::clone(&stop),
            handle,
            ready_receiver,
            Duration::from_millis(20),
        );

        assert!(matches!(result, Err(SessionServiceError::StartupTimeout)));
        assert!(stop.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_millis(500));
        release_sender
            .send(())
            .expect("blocked worker should still be releasable");
    }

    #[test]
    fn notification_signal_sender_must_be_trusted() {
        let signal = BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked")
            .with_sender(":1.42");
        let trusted_senders = vec![":1.42".to_string(), ":1.24".to_string()];

        assert!(notification_signal_sender_is_trusted(
            &signal,
            &trusted_senders
        ));
        assert!(!notification_signal_sender_is_trusted(
            &BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked")
                .with_sender(":1.99"),
            &trusted_senders
        ));
        assert!(!notification_signal_sender_is_trusted(
            &BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked"),
            &trusted_senders,
        ));
    }

    #[test]
    fn gnome_shell_sender_requires_gnome_shell_process_identity() {
        assert!(process_identity_is_gnome_shell(&BusProcessIdentity {
            comm: Some("gnome-shell".to_string()),
            exe_name: Some("gnome-shell".to_string()),
        }));
        assert!(!process_identity_is_gnome_shell(&BusProcessIdentity {
            comm: Some("gnome-shell".to_string()),
            exe_name: Some("not-gnome-shell".to_string()),
        }));
        assert!(!process_identity_is_gnome_shell(&BusProcessIdentity {
            comm: Some("not-gnome-shell".to_string()),
            exe_name: Some("gnome-shell".to_string()),
        }));
        assert!(!process_identity_is_gnome_shell(&BusProcessIdentity {
            comm: None,
            exe_name: Some("gnome-shell".to_string()),
        }));
    }

    #[test]
    fn trusted_senders_only_include_verified_gnome_shell_owner() {
        let senders = trusted_notification_signal_senders_from_candidates(
            ":1.42".to_string(),
            Some((
                ":1.24".to_string(),
                BusProcessIdentity {
                    comm: Some("gnome-shell".to_string()),
                    exe_name: Some("gnome-shell".to_string()),
                },
            )),
        );

        assert_eq!(senders, vec![":1.42".to_string(), ":1.24".to_string()]);

        let senders = trusted_notification_signal_senders_from_candidates(
            ":1.42".to_string(),
            Some((
                ":1.99".to_string(),
                BusProcessIdentity {
                    comm: Some("spoofed-shell".to_string()),
                    exe_name: Some("spoofed-shell".to_string()),
                },
            )),
        );

        assert_eq!(senders, vec![":1.42".to_string()]);
    }

    #[test]
    fn notification_owner_action_signal_from_bus_disables_updates() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        let event = handle_notification_bus_signal(
            &mut dispatcher,
            &action_invoked_bus_signal(DISABLE_UPDATE_CHECKS_ACTION_KEY, ":1.42"),
            &[":1.42".to_string(), ":1.24".to_string()],
        )
        .expect("bus action should succeed");

        assert_eq!(event, Some(SessionNotificationEvent::UpdateChecksDisabled));
        assert_eq!(dispatcher.preferences.disable_calls(), 1);
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn gnome_shell_action_signal_from_bus_disables_updates() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        let event = handle_notification_bus_signal(
            &mut dispatcher,
            &action_invoked_bus_signal(DISABLE_UPDATE_CHECKS_ACTION_KEY, ":1.24"),
            &[":1.42".to_string(), ":1.24".to_string()],
        )
        .expect("bus action should succeed");

        assert_eq!(event, Some(SessionNotificationEvent::UpdateChecksDisabled));
        assert_eq!(dispatcher.preferences.disable_calls(), 1);
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn spoofed_action_signal_from_bus_is_ignored() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        let event = handle_notification_bus_signal(
            &mut dispatcher,
            &action_invoked_bus_signal(DISABLE_UPDATE_CHECKS_ACTION_KEY, ":1.99"),
            &[":1.42".to_string(), ":1.24".to_string()],
        )
        .expect("spoofed signal should be ignored without error");

        assert_eq!(event, None);
        assert_eq!(dispatcher.preferences.disable_calls(), 0);
        assert_eq!(dispatcher.pending_len(), 1);
    }

    #[test]
    fn view_release_action_opens_url_and_clears_pending_state() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: VIEW_RELEASE_ACTION_KEY.to_string(),
            })
            .expect("action should succeed");

        assert_eq!(
            dispatcher.opener.opened(),
            vec!["https://github.test/releases/tag/v1.1.1".to_string()]
        );
        assert_eq!(dispatcher.preferences.disable_calls(), 0);
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn never_notify_again_action_disables_updates_and_clears_pending_state() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: DISABLE_UPDATE_CHECKS_ACTION_KEY.to_string(),
            })
            .expect("opt out action should succeed");

        assert!(dispatcher.opener.opened().is_empty());
        assert_eq!(dispatcher.preferences.disable_calls(), 1);
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn opener_failure_clears_pending_state_and_reports_error() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::failing();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: VIEW_RELEASE_ACTION_KEY.to_string(),
            })
            .expect_err("open should fail");

        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn opt_out_failure_clears_pending_state_and_reports_error() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::failing();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: DISABLE_UPDATE_CHECKS_ACTION_KEY.to_string(),
            })
            .expect_err("settings update should fail");

        assert_eq!(dispatcher.preferences.disable_calls(), 1);
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn closed_signal_clears_pending_state() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::Closed {
                id: NotificationId(7),
                reason: crate::notifications::NotificationCloseReason::Dismissed,
            })
            .expect("close should succeed");

        assert_eq!(dispatcher.pending_len(), 0);
        assert_eq!(dispatcher.recently_closed_len(), 1);
    }

    #[test]
    fn action_after_closed_signal_still_uses_notification_context() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let preferences = RecordingPreferences::default();
        let mut dispatcher =
            SessionUpdateNotificationDispatcher::new(notifier, opener, preferences);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        handle_notification_bus_signal(
            &mut dispatcher,
            &notification_closed_bus_signal(":1.42"),
            &[":1.42".to_string()],
        )
        .expect("close should succeed");
        handle_notification_bus_signal(
            &mut dispatcher,
            &action_invoked_bus_signal(DISABLE_UPDATE_CHECKS_ACTION_KEY, ":1.42"),
            &[":1.42".to_string()],
        )
        .expect("action should succeed");

        assert_eq!(dispatcher.preferences.disable_calls(), 1);
        assert_eq!(dispatcher.pending_len(), 0);
        assert_eq!(dispatcher.recently_closed_len(), 0);
    }
}
