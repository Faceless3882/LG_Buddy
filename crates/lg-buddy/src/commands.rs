use std::env;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Output;
use std::time::Duration;

use crate::brightness::{
    notify_brightness_success_with, read_current_brightness_with, write_brightness_with,
};
use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::events::RuntimeEvent;
use crate::lifecycle::ThreadSleeper;
use crate::lifecycle::{self, JournalctlSleepDetector, NmOnlineNetworkWaiter};
use crate::notifications::{FreedesktopNotifier, Notification, NotificationError, Notifier};
use crate::state::{
    ScreenOwnershipMarker, StateScope, SystemSleepAttemptState, SystemSleepCycleState,
};
use crate::tv::{
    build_tv_client, AudioStatus, OledBrightness, TvClient, TvClientBuildOptions, TvDevice,
    VolumeLevel,
};
use crate::wol::UdpWakeOnLanSender;
use crate::{BrightnessCommand, MuteCommand, RunError, StartupMode, VolumeCommand};

const SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
trait ReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool>;
}

trait BrightnessUi {
    fn prompt_brightness(&self, initial: OledBrightness) -> io::Result<Option<OledBrightness>>;
    fn show_error(&self, title: &str, message: &str) -> io::Result<()>;
}

trait BrightnessCli {
    fn get_brightness(&self) -> Result<OledBrightness, RunError>;
    fn set_brightness(&self, brightness: OledBrightness) -> Result<String, RunError>;
}

struct SystemctlRebootDetector {
    command_path: PathBuf,
}

struct PingReachabilityChecker {
    command_path: PathBuf,
}

struct ZenityBrightnessUi {
    command_path: PathBuf,
}

struct CurrentExeBrightnessCli {
    command_path: PathBuf,
}

struct BrightnessDialogDeps<'a, R, U, B, N> {
    reachability: &'a R,
    ui: &'a U,
    brightness_cli: &'a B,
    notifier: &'a N,
}

impl Default for SystemctlRebootDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for PingReachabilityChecker {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for ZenityBrightnessUi {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemctlRebootDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
        }
    }
}

impl PingReachabilityChecker {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_PING")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ping")),
        }
    }
}

impl ZenityBrightnessUi {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_ZENITY")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("zenity")),
        }
    }
}

impl CurrentExeBrightnessCli {
    fn from_current_exe() -> Result<Self, RunError> {
        Ok(Self {
            command_path: env::current_exe()?,
        })
    }

    fn run(&self, args: &[&str]) -> Result<Output, RunError> {
        ProcessCommand::new(&self.command_path)
            .args(args)
            .output()
            .map_err(RunError::Io)
    }
}

impl BrightnessCli for CurrentExeBrightnessCli {
    fn get_brightness(&self) -> Result<OledBrightness, RunError> {
        let output = self.run(&["brightness", "get"])?;

        if !output.status.success() {
            return Err(RunError::Policy(command_output_message(&output)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        OledBrightness::parse(stdout.trim())
            .map_err(|err| RunError::Policy(format!("invalid output from `brightness get`: {err}")))
    }

    fn set_brightness(&self, brightness: OledBrightness) -> Result<String, RunError> {
        let output = self.run(&["brightness", "set", &brightness.to_string()])?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(RunError::Policy(command_output_message(&output)))
        }
    }
}

impl lifecycle::RebootDetector for SystemctlRebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .arg("list-jobs")
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .any(|line| line.contains("reboot.target") && line.contains("start")))
    }
}

impl ReachabilityChecker for PingReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-c", "1", "-W", "2"])
            .arg(tv_ip.to_string())
            .output()?;

        Ok(output.status.success())
    }
}

impl BrightnessUi for ZenityBrightnessUi {
    fn prompt_brightness(&self, initial: OledBrightness) -> io::Result<Option<OledBrightness>> {
        let output = ProcessCommand::new(&self.command_path)
            .args([
                "--scale",
                "--title=LG TV Brightness",
                "--text=Set OLED Pixel Brightness:",
                "--min-value=0",
                "--max-value=100",
                &format!("--value={initial}"),
                "--step=5",
            ])
            .output()?;

        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let value = OledBrightness::parse(stdout.trim()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid zenity brightness value: {err}"),
            )
        })?;

        Ok(Some(value))
    }

    fn show_error(&self, title: &str, message: &str) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .arg("--error")
            .arg(format!("--title={title}"))
            .arg(format!("--text={message}"))
            .output()?;

        Ok(())
    }
}

fn command_output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return strip_lg_buddy_prefix(&stderr).to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return strip_lg_buddy_prefix(&stdout).to_string();
    }

    format!(
        "brightness command failed with status {}",
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string())
    )
}

fn strip_lg_buddy_prefix(value: &str) -> &str {
    value.strip_prefix("LG Buddy: ").unwrap_or(value)
}

pub fn run_screen_off<W: Write>(writer: &mut W) -> Result<(), RunError> {
    crate::screen::run_screen_off_from_env(writer)
}

pub fn run_sleep_pre<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_sleep_pre_for_event(
        writer,
        RuntimeEvent::from_command(crate::Command::SleepPre)
            .expect("sleep-pre should map to a runtime event"),
    )
}

pub fn run_sleep_pre_for_event<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let cycle_state =
        SystemSleepCycleState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production()
            .stored_token_only()
            .with_command_timeout(SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT),
    )?;
    let sleeper = ThreadSleeper;

    lifecycle::handle_system_suspend_with(
        writer,
        &config,
        &marker,
        &cycle_state,
        &tv_client,
        &sleeper,
        event,
    )
}

pub fn run_sleep<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production().stored_token_only(),
    )?;
    let detector = JournalctlSleepDetector::default();
    let sleeper = ThreadSleeper;

    lifecycle::attempt_legacy_network_manager_sleep_with(
        writer, &config, &marker, &tv_client, &detector, &sleeper,
    )
}

pub fn run_nm_pre_down<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let attempt_state =
        SystemSleepAttemptState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production()
            .stored_token_only()
            .with_command_timeout(SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT),
    )?;
    let sleeper = ThreadSleeper;
    let mut bus = match crate::session_bus::new_system_bus_client() {
        Ok(bus) => bus,
        Err(err) => {
            return fail_open_nm_pre_down_after_system_bus_error(writer, &attempt_state, err);
        }
    };

    crate::sources::linux::network_manager::handle_pre_down_with(
        writer,
        &config,
        &marker,
        &attempt_state,
        &tv_client,
        &sleeper,
        &mut bus,
    )
}

pub fn run_brightness<W: Write>(
    writer: &mut W,
    command: BrightnessCommand,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;

    match command {
        BrightnessCommand::Prompt => {
            let reachability = PingReachabilityChecker::default();
            let ui = ZenityBrightnessUi::default();
            let brightness_cli = CurrentExeBrightnessCli::from_current_exe()?;
            let notifier = FreedesktopNotifier;
            let deps = BrightnessDialogDeps {
                reachability: &reachability,
                ui: &ui,
                brightness_cli: &brightness_cli,
                notifier: &notifier,
            };

            run_brightness_prompt_with(writer, &config, deps)
        }
        BrightnessCommand::Get | BrightnessCommand::Set(_) => {
            let tv_client = build_tv_client(
                &config_path,
                config.tv_ip,
                config.tv_platform,
                TvClientBuildOptions::production(),
            )?;
            run_brightness_command_with(writer, &config, command, &tv_client)
        }
    }
}

pub fn run_volume<W: Write>(writer: &mut W, command: VolumeCommand) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production(),
    )?;

    run_volume_command_with(writer, &config, command, &tv_client)
}

pub fn run_startup<W: Write>(writer: &mut W, mode: StartupMode) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production().stored_token_only(),
    )?;
    if !tv_client.can_authenticate_unattended()? {
        marker.clear()?;
        writeln!(
            writer,
            "LG Buddy Startup: No stored native TV credential; skipping unattended TV control."
        )?;
        return Ok(());
    }
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let network_waiter = NmOnlineNetworkWaiter::default();
    let deps = lifecycle::StartupDeps {
        tv_client: &tv_client,
        wol_sender: &wol_sender,
        sleeper: &sleeper,
        network_waiter: &network_waiter,
    };

    lifecycle::run_startup_with(writer, &config, &marker, deps, mode)
}

pub fn run_system_resume<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let attempt_state =
        SystemSleepAttemptState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production().stored_token_only(),
    )?;
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let network_waiter = NmOnlineNetworkWaiter::default();

    let result = (|| -> Result<(), RunError> {
        match tv_client.can_authenticate_unattended() {
            Ok(true) => lifecycle::restore_after_system_sleep_with(
                writer,
                &config,
                &marker,
                &tv_client,
                &wol_sender,
                &sleeper,
                &network_waiter,
            ),
            Ok(false) => {
                marker.clear()?;
                writeln!(
                    writer,
                    "LG Buddy System Resume: No stored native TV credential; skipping unattended TV control."
                )?;
                Ok(())
            }
            Err(err) => {
                marker.clear()?;
                Err(err.into())
            }
        }
    })();

    let mut cleanup_error = None;

    if let Err(err) = attempt_state.clear() {
        writeln!(
            writer,
            "LG Buddy System Resume: could not clear system sleep attempt marker after resume. {err}"
        )?;
        cleanup_error = Some(err);
    }

    if let Err(err) = attempt_state.clear_outcome() {
        writeln!(
            writer,
            "LG Buddy System Resume: could not clear system sleep cycle state after resume. {err}"
        )?;
        if cleanup_error.is_none() {
            cleanup_error = Some(err);
        }
    }

    if result.is_ok() {
        if let Some(err) = cleanup_error {
            return Err(RunError::Io(err));
        }
    }

    result
}

fn fail_open_nm_pre_down_after_system_bus_error<W: Write, E: std::fmt::Display>(
    writer: &mut W,
    attempt_state: &SystemSleepAttemptState,
    err: E,
) -> Result<(), RunError> {
    writeln!(
        writer,
        "LG Buddy NetworkManager: could not open system bus for logind PreparingForSleep; failing open. {err}"
    )?;
    if let Err(clear_err) = attempt_state.clear() {
        writeln!(
            writer,
            "LG Buddy NetworkManager: could not clear stale system sleep attempt marker after system bus failure. {clear_err}"
        )?;
    }

    Ok(())
}

pub fn run_shutdown<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production().stored_token_only(),
    )?;
    if !tv_client.can_authenticate_unattended()? {
        writeln!(
            writer,
            "LG Buddy Shutdown: No stored native TV credential; skipping unattended TV control."
        )?;
        return Ok(());
    }
    let reboot_detector = SystemctlRebootDetector::default();

    lifecycle::run_shutdown_with(writer, &config, &tv_client, &reboot_detector)
}

pub fn run_screen_on<W: Write>(writer: &mut W) -> Result<(), RunError> {
    crate::screen::run_screen_on_from_env(writer)
}

fn run_brightness_command_with<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    command: BrightnessCommand,
    tv_client: &C,
) -> Result<(), RunError> {
    match command {
        BrightnessCommand::Prompt => unreachable!("prompt is handled by the dialog wrapper"),
        BrightnessCommand::Get => {
            let brightness = read_current_brightness_with(config, tv_client)
                .map_err(|err| RunError::Policy(format!("failed to read brightness: {err}")))?;
            writeln!(writer, "{brightness}")?;
            Ok(())
        }
        BrightnessCommand::Set(brightness) => {
            set_oled_brightness(config, tv_client, brightness)?;
            writeln!(
                writer,
                "LG Buddy Brightness: Set OLED pixel brightness to {brightness}%."
            )?;
            Ok(())
        }
    }
}

fn run_volume_command_with<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    command: VolumeCommand,
    tv_client: &C,
) -> Result<(), RunError> {
    match command {
        VolumeCommand::Get => {
            let status = read_audio_status(config, tv_client)?;
            if status.is_muted() {
                writeln!(writer, "mute")?;
            } else {
                writeln!(writer, "{}", status.volume())?;
            }
        }
        VolumeCommand::Set(volume) => {
            set_volume_and_unmute(config, tv_client, volume)?;
            writeln!(writer, "LG Buddy Volume: Set volume to {volume}.")?;
        }
        VolumeCommand::Up => {
            change_volume_and_unmute(config, tv_client, VolumeChange::Up)?;
            writeln!(writer, "LG Buddy Volume: Increased volume.")?;
        }
        VolumeCommand::Down => {
            change_volume_and_unmute(config, tv_client, VolumeChange::Down)?;
            writeln!(writer, "LG Buddy Volume: Decreased volume.")?;
        }
        VolumeCommand::Mute(command) => {
            let muted = match command {
                MuteCommand::Toggle => !read_audio_status(config, tv_client)?.is_muted(),
                MuteCommand::On => true,
                MuteCommand::Off => false,
            };
            set_muted(config, tv_client, muted)?;
            let state = if muted { "Muted" } else { "Unmuted" };
            writeln!(writer, "LG Buddy Volume: {state}.")?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeChange {
    Up,
    Down,
}

fn read_audio_status<C: TvClient>(config: &Config, tv_client: &C) -> Result<AudioStatus, RunError> {
    TvDevice::new(tv_client, config.tv_ip)
        .audio()
        .status()
        .map_err(|err| RunError::Policy(format!("failed to read volume: {err}")))
}

fn set_volume_and_unmute<C: TvClient>(
    config: &Config,
    tv_client: &C,
    volume: VolumeLevel,
) -> Result<(), RunError> {
    let audio = TvDevice::new(tv_client, config.tv_ip).audio();
    audio
        .set_volume(volume)
        .map_err(|err| RunError::Policy(format!("failed to set volume: {err}")))?;
    audio
        .set_muted(false)
        .map_err(|err| RunError::Policy(format!("volume was changed, but unmuting failed: {err}")))
}

fn change_volume_and_unmute<C: TvClient>(
    config: &Config,
    tv_client: &C,
    change: VolumeChange,
) -> Result<(), RunError> {
    let audio = TvDevice::new(tv_client, config.tv_ip).audio();
    let result = match change {
        VolumeChange::Up => audio.volume_up(),
        VolumeChange::Down => audio.volume_down(),
    };
    result.map_err(|err| RunError::Policy(format!("failed to change volume: {err}")))?;
    audio
        .set_muted(false)
        .map_err(|err| RunError::Policy(format!("volume was changed, but unmuting failed: {err}")))
}

fn set_muted<C: TvClient>(config: &Config, tv_client: &C, muted: bool) -> Result<(), RunError> {
    TvDevice::new(tv_client, config.tv_ip)
        .audio()
        .set_muted(muted)
        .map_err(|err| RunError::Policy(format!("failed to set mute: {err}")))
}

fn run_brightness_prompt_with<
    W: Write,
    R: ReachabilityChecker,
    U: BrightnessUi,
    B: BrightnessCli,
    N: Notifier,
>(
    writer: &mut W,
    config: &Config,
    deps: BrightnessDialogDeps<'_, R, U, B, N>,
) -> Result<(), RunError> {
    match deps.reachability.is_reachable(config.tv_ip) {
        Ok(true) => {}
        Ok(false) => {
            let message = format!("TV is not reachable at {}.", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
        Err(err) => {
            let message = format!("Could not check TV reachability at {}. {err}", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
    }

    let initial_brightness = deps
        .brightness_cli
        .get_brightness()
        .unwrap_or(OledBrightness::DEFAULT);

    let Some(brightness) = deps.ui.prompt_brightness(initial_brightness)? else {
        return Ok(());
    };

    match deps.brightness_cli.set_brightness(brightness) {
        Ok(stdout) => {
            write!(writer, "{stdout}")?;
            notify_brightness_success(deps.notifier, brightness)?;
            Ok(())
        }
        Err(err) => Err(notify_brightness_failure(deps.notifier, err)),
    }
}

fn notify_brightness_success<N: Notifier>(
    notifier: &N,
    brightness: OledBrightness,
) -> Result<(), RunError> {
    notify_brightness_success_with(notifier, brightness).map_err(|err| {
        RunError::Policy(format!(
            "brightness was set to {brightness}%, but desktop notification failed: {err}"
        ))
    })
}

fn notify_brightness_failure<N: Notifier>(notifier: &N, primary: RunError) -> RunError {
    match notifier.notify(&Notification::new("LG TV", "Failed to set brightness")) {
        Ok(_) => primary,
        Err(notification_err) => append_notification_failure(primary, notification_err),
    }
}

fn append_notification_failure(primary: RunError, notification_err: NotificationError) -> RunError {
    RunError::NotificationAfterPrimary {
        primary: Box::new(primary),
        notification: notification_err,
    }
}

fn set_oled_brightness<C: TvClient>(
    config: &Config,
    tv_client: &C,
    brightness: OledBrightness,
) -> Result<(), RunError> {
    write_brightness_with(config, tv_client, brightness)
        .map_err(|err| RunError::Policy(format!("failed to set brightness: {err}")))
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        run_brightness_command_with, run_brightness_prompt_with, BrightnessCli,
        BrightnessDialogDeps, BrightnessUi, ReachabilityChecker,
    };
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenIdleBlankPolicy, ScreenRestorePolicy,
        SystemSleepWakePolicy,
    };
    use crate::lifecycle::{
        run_shutdown_with, run_startup_with, NetworkWaiter, RebootDetector, SleepRequestDetector,
        Sleeper, StartupDeps,
    };
    use crate::notifications::{Notification, NotificationError, NotificationId, Notifier};
    use crate::screen::{run_screen_off_with, run_screen_on_with};
    use crate::state::ScreenOwnershipMarker;
    use crate::state::SystemSleepAttemptState;
    use crate::tv::{BscpylgtvCommandClient, OledBrightness};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use crate::{BrightnessCommand, RunError, StartupMode};
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::net::Ipv4Addr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use support::MockBscpylgtv;

    #[cfg(unix)]
    fn set_modified_time(path: &Path, modified: SystemTime) {
        let duration = modified
            .duration_since(UNIX_EPOCH)
            .expect("modified time should be after the unix epoch");
        let path =
            CString::new(path.as_os_str().as_bytes()).expect("path should not contain nul bytes");
        let times = [
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
        ];

        let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(
            result,
            0,
            "failed to set file timestamps: {}",
            io::Error::last_os_error()
        );
    }

    #[cfg(not(unix))]
    fn set_modified_time(_path: &Path, _modified: SystemTime) {
        panic!("set_modified_time is only implemented for unix test targets");
    }

    #[test]
    fn matching_input_blanks_screen_and_sets_marker() {
        let temp_dir = TestDir::new("screen-off-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-success-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off"]);
        assert!(rendered(&output).contains("Screen blank command succeeded."));
    }

    #[test]
    fn nm_pre_down_system_bus_failure_clears_stale_attempt_marker() {
        let temp_dir = TestDir::new("nm-pre-down-bus-failure");
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create stale attempt marker");

        let mut output = Vec::new();
        super::fail_open_nm_pre_down_after_system_bus_error(
            &mut output,
            &attempt_state,
            "simulated bus failure",
        )
        .expect("fail-open path should succeed");

        assert!(!attempt_state.exists());
        assert!(rendered(&output).contains("could not open system bus"));
    }

    #[test]
    fn matching_input_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-fallback");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-fallback-tv");
        mock.set_input("HDMI_3");
        mock.queue_error("turn_screen_off", 1, "blank failed\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off", "power_off"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Screen blank failed."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn get_input_failure_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-get-input-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-get-input-failure-tv");
        mock.queue_error("get_input", 1, "unreachable\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Could not query TV input."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn different_input_skips_and_clears_marker() {
        let temp_dir = TestDir::new("screen-off-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale marker");
        let mock = MockBscpylgtv::new("screen-off-skip-tv");
        mock.set_input("HDMI_4");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("Skipping idle action."));
    }

    #[test]
    fn failed_fallback_does_not_set_marker() {
        let temp_dir = TestDir::new("screen-off-fallback-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-fallback-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("turn_screen_off", 1, "blank failed\n");
        mock.queue_error("power_off", 1, "power failed\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should still return ok");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off", "power_off"]);
        assert!(rendered(&output).contains("Fallback power_off failed."));
    }

    #[test]
    fn screen_on_skips_when_marker_is_missing() {
        let temp_dir = TestDir::new("screen-on-no-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-no-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("missing marker should skip");

        assert_call_commands(&mock, &[]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("State file not found."));
    }

    #[test]
    fn screen_on_aggressive_mode_restores_without_marker() {
        let temp_dir = TestDir::new("screen-on-aggressive-no-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-aggressive-no-marker-tv");
        mock.set_screen_on(false);
        mock.queue_error("turn_screen_on", 1, "offline\n");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config_with_restore_policy(HdmiInput::Hdmi2, ScreenRestorePolicy::Aggressive),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("aggressive mode should restore without a marker");

        assert!(!marker.exists());
        assert_call_commands(
            &mock,
            &[
                "turn_screen_on",
                "get_power_state",
                "set_input",
                "set_input",
                "get_power_state",
            ],
        );
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Aggressive restore policy is enabled"));
        assert!(rendered.contains("Wake attempt 2 succeeded."));
        assert!(rendered.contains("Clearing wake state."));
    }

    #[test]
    fn screen_on_restores_even_when_marker_is_old() {
        let temp_dir = TestDir::new("screen-on-old-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        set_modified_time(
            marker.path(),
            SystemTime::now() - Duration::from_secs((12 * 60 * 60) + 1),
        );
        let mock = MockBscpylgtv::new("screen-on-old-marker-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("old marker should still restore");

        assert!(
            !marker.exists(),
            "successful restore should clear the marker"
        );
        assert_call_commands(&mock, &["turn_screen_on", "get_power_state"]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("Screen unblank succeeded."));
    }

    #[test]
    fn screen_on_unblanks_and_clears_marker() {
        let temp_dir = TestDir::new("screen-on-unblank");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-unblank-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("turn_screen_on should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on", "get_power_state"]);
        assert!(wol.calls().is_empty());
        assert!(rendered(&output).contains("Screen unblank succeeded."));
    }

    #[test]
    fn screen_on_treats_already_active_unblank_as_success() {
        let temp_dir = TestDir::new("screen-on-substate-mismatch");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-substate-mismatch-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("active power-state readback should satisfy unblank");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on", "get_power_state"]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        let rendered = rendered(&output);
        assert!(rendered.contains("Screen unblank succeeded."));
        assert!(!rendered.contains("Falling back to full wake."));
    }

    #[test]
    fn screen_on_falls_back_to_wake_and_retries_until_success() {
        let temp_dir = TestDir::new("screen-on-wake-retry-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-wake-retry-success-tv");
        mock.set_screen_on(false);
        mock.queue_error("turn_screen_on", 1, "offline\n");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("wake retry should succeed");

        assert!(!marker.exists());
        assert_call_commands(
            &mock,
            &[
                "turn_screen_on",
                "get_power_state",
                "set_input",
                "set_input",
                "get_power_state",
            ],
        );
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Sending initial Wake-on-LAN packet"));
        assert!(rendered.contains("Wake attempt 1 failed."));
        assert!(rendered.contains("Wake attempt 2 succeeded."));
    }

    #[test]
    fn screen_on_returns_error_and_preserves_marker_after_exhausting_retries() {
        let temp_dir = TestDir::new("screen-on-wake-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-wake-retry-failure-tv");
        mock.set_screen_on(false);
        mock.queue_error("turn_screen_on", 1, "offline\n");
        for _ in 0..6 {
            mock.queue_error("set_input", 1, "not ready\n");
        }
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let err = run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect_err("exhausted retries should fail");

        assert!(marker.exists());
        assert_eq!(mock.calls().len(), 8);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(matches!(err, crate::RunError::Policy(_)));
        assert!(rendered(&output).contains("Wake failed after 6 attempts."));
    }

    #[test]
    fn screen_on_aggressive_mode_returns_error_without_creating_marker_after_exhausting_retries() {
        let temp_dir = TestDir::new("screen-on-aggressive-wake-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-aggressive-wake-retry-failure-tv");
        mock.set_screen_on(false);
        mock.queue_error("turn_screen_on", 1, "offline\n");
        for _ in 0..6 {
            mock.queue_error("set_input", 1, "not ready\n");
        }
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let err = run_screen_on_with(
            &mut output,
            &sample_config_with_restore_policy(HdmiInput::Hdmi2, ScreenRestorePolicy::Aggressive),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect_err("aggressive mode should still fail after exhausting retries");

        assert!(!marker.exists());
        assert_eq!(mock.calls().len(), 8);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(matches!(err, crate::RunError::Policy(_)));
        let rendered = rendered(&output);
        assert!(rendered.contains("Aggressive restore policy is enabled"));
        assert!(rendered.contains("Wake failed after 6 attempts."));
    }

    #[test]
    fn startup_wake_mode_skips_without_system_marker() {
        let temp_dir = TestDir::new("startup-wake-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-wake-skip-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Wake,
        )
        .expect("wake mode without marker should skip");

        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert_eq!(network.calls(), 0);
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("TV was not on our input. Skipping."));
    }

    #[test]
    fn startup_wake_mode_restores_without_system_marker_in_aggressive_mode() {
        let temp_dir = TestDir::new("startup-wake-aggressive-no-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-wake-aggressive-no-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config_with_restore_policy(HdmiInput::Hdmi2, ScreenRestorePolicy::Aggressive),
            &marker,
            deps,
            StartupMode::Wake,
        )
        .expect("aggressive wake mode without marker should restore");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(wol.calls().len(), 1);
        assert_eq!(sleeper.durations(), vec![Duration::from_secs(6)]);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Aggressive restore policy is enabled"));
        assert!(rendered.contains("Wake from sleep: Restoring display state."));
        assert!(rendered.contains("TV turned on and set to HDMI_2."));
    }

    #[test]
    fn startup_auto_mode_treats_missing_marker_as_boot() {
        let temp_dir = TestDir::new("startup-auto-boot");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-auto-boot-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("auto boot should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(wol.calls().len(), 1);
        assert_eq!(sleeper.durations(), vec![Duration::from_secs(6)]);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Cold boot: Turning TV on and switching to HDMI_4."));
        assert!(rendered.contains("TV turned on and set to HDMI_4."));
    }

    #[test]
    fn startup_auto_mode_restores_when_system_marker_exists() {
        let temp_dir = TestDir::new("startup-auto-wake");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("startup-auto-wake-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("auto wake should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        assert!(rendered(&output).contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
    }

    #[test]
    fn startup_boot_mode_clears_existing_marker_before_restore() {
        let temp_dir = TestDir::new("startup-boot-clears-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale system marker");
        let mock = MockBscpylgtv::new("startup-boot-clears-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("boot mode should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        assert!(rendered(&output).contains("Cold boot: Turning TV on and switching to HDMI_3."));
    }

    #[test]
    fn startup_ignores_network_wait_failures() {
        let temp_dir = TestDir::new("startup-network-wait-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-network-wait-failure-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::failing(io::ErrorKind::TimedOut, "network offline");
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup should continue even if network wait fails");

        assert_eq!(network.calls(), 1);
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        assert!(rendered(&output).contains("TV turned on and set to HDMI_2."));
    }

    #[test]
    fn startup_retries_until_set_input_succeeds() {
        let temp_dir = TestDir::new("startup-retry-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-retry-success-tv");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup retry should succeed");

        assert_eq!(network.calls(), 1);
        assert_call_commands(&mock, &["set_input", "set_input", "get_power_state"]);
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Attempt 1 failed, retrying in 2s..."));
        assert!(rendered.contains("TV turned on and set to HDMI_2."));
    }

    #[test]
    fn startup_logs_failure_after_exhausting_retries_and_leaves_marker_cleared() {
        let temp_dir = TestDir::new("startup-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("startup-retry-failure-tv");
        for _ in 0..6 {
            mock.queue_error("set_input", 1, "not ready\n");
        }
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("startup should still succeed after exhausting retries");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(mock.calls().len(), 6);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(rendered(&output).contains("set_input failed after 6 attempts"));
    }

    #[test]
    fn brightness_dialog_uses_cli_get_and_set_then_notifies() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(65);
        let brightness_cli = FakeBrightnessCli::success(72)
            .with_set_stdout("LG Buddy Brightness: Set OLED pixel brightness to 65%.\n");
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("brightness should succeed");

        assert_eq!(ui.initial_values(), vec![72]);
        assert!(ui.error_messages().is_empty());
        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Brightness set to 65%".to_string())]
        );
        assert_eq!(
            brightness_cli.calls(),
            vec![FakeBrightnessCliCall::Get, FakeBrightnessCliCall::Set(65),]
        );
        assert!(rendered(&output).contains("Set OLED pixel brightness to 65%."));
    }

    #[test]
    fn brightness_fails_after_success_when_notification_delivery_fails() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(65);
        let brightness_cli = FakeBrightnessCli::success(72)
            .with_set_stdout("LG Buddy Brightness: Set OLED pixel brightness to 65%.\n");
        let notifier = RecordingNotifier::failing("bus unavailable");
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("notification failure after success should fail");

        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Brightness set to 65%".to_string())]
        );
        assert_eq!(
            brightness_cli.calls(),
            vec![FakeBrightnessCliCall::Get, FakeBrightnessCliCall::Set(65),]
        );
        assert!(rendered(&output).contains("Set OLED pixel brightness to 65%."));
        assert!(err
            .to_string()
            .contains("brightness was set to 65%, but desktop notification failed"));
    }

    #[test]
    fn brightness_get_prints_current_oled_brightness() {
        let mock = MockBscpylgtv::new("brightness-get-tv");
        mock.set_backlight(72);
        let client = client_for_mock(&mock);
        let mut output = Vec::new();
        run_brightness_command_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            BrightnessCommand::Get,
            &client,
        )
        .expect("brightness get should succeed");

        assert_eq!(rendered(&output).trim(), "72");
        assert_call_commands(&mock, &["get_picture_settings"]);
    }

    #[test]
    fn brightness_set_updates_oled_brightness_without_dialog() {
        let mock = MockBscpylgtv::new("brightness-set-tv");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_brightness_command_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            BrightnessCommand::Set(OledBrightness::new(61).expect("valid brightness")),
            &client,
        )
        .expect("brightness set should succeed");

        assert_eq!(mock.state_snapshot().backlight, 61);
        assert_call_commands(&mock, &["set_settings"]);
        assert!(rendered(&output).contains("Set OLED pixel brightness to 61%."));
    }

    #[test]
    fn brightness_returns_ok_when_dialog_is_cancelled() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::cancelled();
        let brightness_cli = FakeBrightnessCli::success(50);
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("cancel should exit cleanly");

        assert_eq!(ui.initial_values(), vec![50]);
        assert_eq!(brightness_cli.calls(), vec![FakeBrightnessCliCall::Get]);
        assert!(notifier.messages().is_empty());
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn brightness_shows_error_and_fails_when_tv_is_unreachable() {
        let reachability = FakeReachabilityChecker::unreachable();
        let ui = FakeBrightnessUi::selected(50);
        let brightness_cli = FakeBrightnessCli::success(50);
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("unreachable tv should fail");

        assert!(matches!(err, crate::RunError::Policy(_)));
        assert!(brightness_cli.calls().is_empty());
        assert!(notifier.messages().is_empty());
        assert_eq!(
            ui.error_messages(),
            vec![(
                "LG Buddy".to_string(),
                "TV is not reachable at 192.0.2.42.".to_string(),
            )]
        );
    }

    #[test]
    fn brightness_defaults_to_fifty_when_current_brightness_query_fails() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::cancelled();
        let brightness_cli = FakeBrightnessCli::get_error("offline");
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("fallback to default brightness should still allow cancellation");

        assert_eq!(ui.initial_values(), vec![50]);
        assert_eq!(brightness_cli.calls(), vec![FakeBrightnessCliCall::Get]);
        assert!(notifier.messages().is_empty());
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn brightness_notifies_and_fails_when_tv_update_fails() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(30);
        let brightness_cli = FakeBrightnessCli::success(50).with_set_error("offline");
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("tv command failure should fail");

        assert!(matches!(err, crate::RunError::Policy(_)));
        assert_eq!(ui.initial_values(), vec![50]);
        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Failed to set brightness".to_string())]
        );
        assert_eq!(
            brightness_cli.calls(),
            vec![FakeBrightnessCliCall::Get, FakeBrightnessCliCall::Set(30),]
        );
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn brightness_preserves_tv_failure_when_failure_notification_fails() {
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(30);
        let brightness_cli = FakeBrightnessCli::success(50).with_set_error("offline");
        let notifier = RecordingNotifier::failing("bus unavailable");
        let deps = BrightnessDialogDeps {
            reachability: &reachability,
            ui: &ui,
            brightness_cli: &brightness_cli,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_prompt_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("tv and notification failure should fail");

        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Failed to set brightness".to_string())]
        );
        assert_eq!(
            err.to_string(),
            "offline; additionally, desktop notification failed: desktop notification service error: bus unavailable"
        );
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn shutdown_ignores_reboot() {
        let mock = MockBscpylgtv::new("shutdown-ignores-reboot-tv");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::pending();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("reboot should be ignored");

        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("Reboot; ignoring"));
    }

    #[test]
    fn shutdown_powers_off_when_configured_input_is_active() {
        let mock = MockBscpylgtv::new("shutdown-match-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &client,
            &detector,
        )
        .expect("matching input should power off");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("TV is on HDMI_3. Turning off for shutdown."));
    }

    #[test]
    fn shutdown_skips_when_tv_is_on_different_input() {
        let mock = MockBscpylgtv::new("shutdown-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &client,
            &detector,
        )
        .expect("nonmatching input should skip");

        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("TV is on HDMI_1 (not HDMI_4). Skipping."));
    }

    #[test]
    fn shutdown_falls_back_to_power_off_when_input_query_fails() {
        let mock = MockBscpylgtv::new("shutdown-fallback-tv");
        mock.queue_error("get_input", 1, "offline\n");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("query failure should still power off");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("Could not query TV input. Proceeding with power_off."));
    }

    #[test]
    fn shutdown_logs_power_off_failure_but_does_not_error() {
        let mock = MockBscpylgtv::new("shutdown-power-off-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "already off\n");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("power_off failure should not abort shutdown");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("power_off failed, continuing shutdown."));
    }

    #[test]
    fn sleep_pre_powers_off_and_sets_system_marker_on_matching_input() {
        let temp_dir = TestDir::new("sleep-pre-match");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-pre-match-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_system_sleep_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("Turning off for sleep."));
    }

    #[test]
    fn sleep_pre_skips_and_clears_marker_on_different_input() {
        let temp_dir = TestDir::new("sleep-pre-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("sleep-pre-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_system_sleep_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("Skipping."));
    }

    #[test]
    fn sleep_pre_falls_back_to_power_off_when_input_query_keeps_failing() {
        let temp_dir = TestDir::new("sleep-pre-fallback");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-pre-fallback-tv");
        mock.queue_error("get_input", 1, "offline\n");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_system_sleep_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("Attempting power_off fallback."));
    }

    #[test]
    fn sleep_skips_when_networkmanager_is_not_entering_sleep() {
        let temp_dir = TestDir::new("sleep-noop");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-noop-tv");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::clear();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_legacy_network_manager_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep noop should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn sleep_powers_off_and_sets_marker_when_sleep_is_requested() {
        let temp_dir = TestDir::new("sleep-match");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-match-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::pending();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_legacy_network_manager_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep should power off");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn sleep_skips_when_tv_is_on_different_input() {
        let temp_dir = TestDir::new("sleep-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("sleep-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::pending();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        crate::lifecycle::attempt_legacy_network_manager_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).is_empty());
    }

    fn sample_config(input: HdmiInput) -> Config {
        sample_config_with_restore_policy(input, ScreenRestorePolicy::MarkerOnly)
    }

    fn sample_config_with_restore_policy(
        input: HdmiInput,
        screen_restore_policy: ScreenRestorePolicy,
    ) -> Config {
        Config {
            tv_ip: "192.0.2.42".parse::<Ipv4Addr>().expect("parse ipv4"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("parse mac"),
            input,
            tv_platform: crate::config::TvPlatform::Bscpylgtv,
            screen_backend: ScreenBackend::Auto,
            screen_idle_blank: ScreenIdleBlankPolicy::Enabled,
            screen_idle_timeout: 300,
            screen_restore_policy,
            system_sleep_wake_policy: SystemSleepWakePolicy::Enabled,
        }
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    fn client_for_mock(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(
            std::net::Ipv4Addr::new(192, 0, 2, 42),
            mock.command_path(),
            mock.command_args(),
        )
    }

    fn assert_call_commands(mock: &MockBscpylgtv, expected: &[&str]) {
        let actual = mock
            .calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>();
        let expected = expected
            .iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[derive(Default)]
    struct RecordingWakeOnLanSender {
        calls: RefCell<Vec<MacAddress>>,
    }

    impl RecordingWakeOnLanSender {
        fn calls(&self) -> Vec<MacAddress> {
            self.calls.borrow().clone()
        }
    }

    impl WakeOnLanSender for RecordingWakeOnLanSender {
        fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError> {
            self.calls.borrow_mut().push(*mac);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingSleeper {
        durations: RefCell<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn durations(&self) -> Vec<Duration> {
            self.durations.borrow().clone()
        }
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
        }
    }

    impl crate::screen::Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
        }
    }

    struct FakeNetworkWaiter {
        result: io::Result<()>,
        calls: RefCell<u32>,
    }

    impl FakeNetworkWaiter {
        fn clear() -> Self {
            Self {
                result: Ok(()),
                calls: RefCell::new(0),
            }
        }

        fn failing(kind: io::ErrorKind, message: &str) -> Self {
            Self {
                result: Err(io::Error::new(kind, message.to_string())),
                calls: RefCell::new(0),
            }
        }

        fn calls(&self) -> u32 {
            *self.calls.borrow()
        }
    }

    impl NetworkWaiter for FakeNetworkWaiter {
        fn wait_for_network(&self) -> io::Result<()> {
            *self.calls.borrow_mut() += 1;

            match &self.result {
                Ok(()) => Ok(()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct FakeReachabilityChecker {
        reachable: io::Result<bool>,
    }

    impl FakeReachabilityChecker {
        fn reachable() -> Self {
            Self {
                reachable: Ok(true),
            }
        }

        fn unreachable() -> Self {
            Self {
                reachable: Ok(false),
            }
        }
    }

    impl ReachabilityChecker for FakeReachabilityChecker {
        fn is_reachable(&self, _tv_ip: Ipv4Addr) -> io::Result<bool> {
            match &self.reachable {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeBrightnessCliCall {
        Get,
        Set(u8),
    }

    struct FakeBrightnessCli {
        get_result: Result<OledBrightness, String>,
        set_result: Result<String, String>,
        calls: RefCell<Vec<FakeBrightnessCliCall>>,
    }

    impl FakeBrightnessCli {
        fn success(current: u8) -> Self {
            Self {
                get_result: Ok(
                    OledBrightness::new(current).expect("fake brightness value should be valid")
                ),
                set_result: Ok(String::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn get_error(message: &str) -> Self {
            Self {
                get_result: Err(message.to_string()),
                set_result: Ok(String::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_set_error(mut self, message: &str) -> Self {
            self.set_result = Err(message.to_string());
            self
        }

        fn with_set_stdout(mut self, stdout: &str) -> Self {
            self.set_result = Ok(stdout.to_string());
            self
        }

        fn calls(&self) -> Vec<FakeBrightnessCliCall> {
            self.calls.borrow().clone()
        }
    }

    impl BrightnessCli for FakeBrightnessCli {
        fn get_brightness(&self) -> Result<OledBrightness, RunError> {
            self.calls.borrow_mut().push(FakeBrightnessCliCall::Get);
            self.get_result
                .as_ref()
                .copied()
                .map_err(|message| RunError::Policy(message.clone()))
        }

        fn set_brightness(&self, brightness: OledBrightness) -> Result<String, RunError> {
            self.calls
                .borrow_mut()
                .push(FakeBrightnessCliCall::Set(brightness.as_percent()));
            self.set_result
                .as_ref()
                .cloned()
                .map_err(|message| RunError::Policy(message.clone()))
        }
    }

    struct FakeBrightnessUi {
        selection: io::Result<Option<OledBrightness>>,
        initial_values: RefCell<Vec<u8>>,
        error_messages: RefCell<Vec<(String, String)>>,
    }

    impl FakeBrightnessUi {
        fn selected(value: u8) -> Self {
            Self {
                selection: Ok(Some(
                    OledBrightness::new(value).expect("fake brightness value should be valid"),
                )),
                initial_values: RefCell::new(Vec::new()),
                error_messages: RefCell::new(Vec::new()),
            }
        }

        fn cancelled() -> Self {
            Self {
                selection: Ok(None),
                initial_values: RefCell::new(Vec::new()),
                error_messages: RefCell::new(Vec::new()),
            }
        }

        fn initial_values(&self) -> Vec<u8> {
            self.initial_values.borrow().clone()
        }

        fn error_messages(&self) -> Vec<(String, String)> {
            self.error_messages.borrow().clone()
        }
    }

    impl BrightnessUi for FakeBrightnessUi {
        fn prompt_brightness(&self, initial: OledBrightness) -> io::Result<Option<OledBrightness>> {
            self.initial_values.borrow_mut().push(initial.as_percent());
            match &self.selection {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }

        fn show_error(&self, title: &str, message: &str) -> io::Result<()> {
            self.error_messages
                .borrow_mut()
                .push((title.to_string(), message.to_string()));
            Ok(())
        }
    }

    struct RecordingNotifier {
        messages: RefCell<Vec<(String, String)>>,
        result: Result<NotificationId, NotificationError>,
    }

    impl RecordingNotifier {
        fn failing(message: &str) -> Self {
            Self {
                messages: RefCell::new(Vec::new()),
                result: Err(NotificationError::Transport(message.to_string())),
            }
        }

        fn messages(&self) -> Vec<(String, String)> {
            self.messages.borrow().clone()
        }
    }

    impl Default for RecordingNotifier {
        fn default() -> Self {
            Self {
                messages: RefCell::new(Vec::new()),
                result: Ok(NotificationId(1)),
            }
        }
    }

    impl Notifier for RecordingNotifier {
        fn capabilities(
            &self,
        ) -> Result<crate::notifications::NotificationCapabilities, NotificationError> {
            Ok(crate::notifications::NotificationCapabilities { actions: true })
        }

        fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
            self.messages
                .borrow_mut()
                .push((notification.summary.clone(), notification.body.clone()));
            self.result.clone()
        }
    }

    struct FakeRebootDetector {
        pending: io::Result<bool>,
    }

    impl FakeRebootDetector {
        fn clear() -> Self {
            Self { pending: Ok(false) }
        }

        fn pending() -> Self {
            Self { pending: Ok(true) }
        }
    }

    impl RebootDetector for FakeRebootDetector {
        fn is_reboot_pending(&self) -> io::Result<bool> {
            match &self.pending {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct FakeSleepRequestDetector {
        requested: io::Result<bool>,
    }

    impl FakeSleepRequestDetector {
        fn clear() -> Self {
            Self {
                requested: Ok(false),
            }
        }

        fn pending() -> Self {
            Self {
                requested: Ok(true),
            }
        }
    }

    impl SleepRequestDetector for FakeSleepRequestDetector {
        fn is_sleep_requested(&self) -> io::Result<bool> {
            match &self.requested {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-{label}-{}-{timestamp}-{unique}",
                process::id()
            ));

            fs::create_dir_all(&path).expect("create test temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
