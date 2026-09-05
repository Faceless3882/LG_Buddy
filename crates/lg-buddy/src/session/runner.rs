use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{
    configured_backend_from_env_or_config, resolve_backend_with_probe, BackendDetectionError,
    BackendResolution, BackendSelectionError, SystemBackendProbe, SWAYIDLE_DEPRECATION_NOTICE,
};
use crate::commands::{run_sleep_pre_for_event, run_system_resume};
use crate::config::{
    load_config, normalize_idle_timeout_secs, parse_config_entries, parse_idle_timeout_secs,
    resolve_config_path_from_env, ConfigPathError, ScreenBackend, ScreenIdleBlankPolicy,
    DEFAULT_IDLE_TIMEOUT,
};
use crate::events::{EventSource, RuntimeEvent};
use crate::lifecycle::LifecycleEvent;
use crate::session::gamepad::{
    open_system_gamepad_activity_source, open_system_gamepad_device_event_monitor,
    SystemGamepadActivitySource, SystemGamepadDeviceEventMonitor,
};
use crate::session::inactivity::{InactivityDecision, InactivityEngine, InactivityObservation};
use crate::session::{SessionBackend, SessionBackendError, SessionEvent};
use crate::session_bus::{
    new_session_bus_client, new_system_bus_client, BusSignal, BusSignalMatch, SessionBusClient,
    DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};
use crate::session_notifications::spawn_session_notification_service;
use crate::sources::desktop::gnome::{
    current_idle_monitor_idletime_ms, map_screen_saver_signal, resolve_screen_saver_owner,
    screen_saver_owner_changed, GnomeBackend, SystemGnomeProbe, GNOME_SCREEN_SAVER_INTERFACE,
    GNOME_SCREEN_SAVER_PATH, GNOME_SHELL_NAME,
};
use crate::sources::desktop::wayland::run_wayland_activity_monitor;
use crate::sources::linux::logind::{
    acquire_sleep_delay_inhibitor, add_locked_hint_signal_match, add_logind_owner_signal_match,
    add_logind_signal_match, locked_hint, logind_owner_changed, map_locked_hint_change,
    map_prepare_for_sleep_signal, resolve_current_graphical_session, resolve_logind_owner,
    LockedHintChange, LockedHintTracker, LogindSession, LogindSessionError,
};
use crate::state::{ScreenOwnershipMarker, StateScope};
use crate::RunError;

const GNOME_WAIT_TIMEOUT_SECS: u64 = 15;
const GNOME_BUS_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const GNOME_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const GNOME_DESKTOP_ACTIVITY_WINDOW: Duration = Duration::from_millis(500);
const GAMEPAD_ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const GAMEPAD_ACTIVITY_REFRESH_DEBOUNCE: Duration = Duration::from_millis(250);
const GAMEPAD_ACTIVITY_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const GAMEPAD_ACTIVITY_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);
const GAMEPAD_ACTIVITY_SEND_INTERVAL: Duration = Duration::from_millis(500);
const LOGIND_LIFECYCLE_PROCESS_INTERVAL: Duration = Duration::from_secs(5);
const LOGIND_LIFECYCLE_TEST_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const LOGIND_LOCK_PROCESS_INTERVAL: Duration = Duration::from_millis(100);
const SESSION_AGENT_BACKEND_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV: &str = "LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS";
const LIFECYCLE_MONITOR_TEST_TIMEOUT_SECS_ENV: &str =
    "LG_BUDDY_LIFECYCLE_MONITOR_TEST_TIMEOUT_SECS";
const LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV: &str = "LG_BUDDY_LIFECYCLE_MONITOR_TEST_EVENT_LIMIT";
const GAMEPAD_ACTIVITY_SOURCE_ENV: &str = "LG_BUDDY_GAMEPAD_ACTIVITY_SOURCE";
const GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV: &str = "LG_BUDDY_GAMEPAD_ACTIVITY_TEST_AFTER_SECS";

pub trait SessionActionExecutor {
    fn screen_off(&mut self, event: RuntimeEvent) -> Result<String, RunError>;
    fn screen_on(&mut self, event: RuntimeEvent) -> Result<String, RunError>;
    fn before_sleep(&mut self, event: RuntimeEvent) -> Result<String, RunError>;
    fn after_resume(&mut self, event: RuntimeEvent) -> Result<String, RunError>;

    fn after_resume_streaming<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<(), RunError> {
        let output = self.after_resume(event)?;
        write_command_output(writer, &output)?;
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeActionExecutor;

impl SessionActionExecutor for RuntimeActionExecutor {
    fn screen_off(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
        run_action(|writer| crate::screen::run_screen_off_from_env_for_event(writer, event))
    }

    fn screen_on(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
        run_action(|writer| crate::screen::run_screen_on_from_env_for_event(writer, event))
    }

    fn before_sleep(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
        run_action(|writer| run_sleep_pre_for_event(writer, event))
    }

    fn after_resume(&mut self, _event: RuntimeEvent) -> Result<String, RunError> {
        run_action(run_system_resume)
    }

    fn after_resume_streaming<W: Write>(
        &mut self,
        writer: &mut W,
        _event: RuntimeEvent,
    ) -> Result<(), RunError> {
        run_system_resume(writer)
    }
}

#[derive(Debug)]
pub enum SessionRunnerError {
    Io(String),
    BackendUnavailable(SessionBackendError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
    UnsupportedBackend {
        backend: ScreenBackend,
        reason: &'static str,
    },
    Action(RunError),
    Failed {
        backend: ScreenBackend,
        message: String,
    },
}

impl fmt::Display for SessionRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "{message}"),
            Self::BackendUnavailable(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
            Self::UnsupportedBackend { backend, reason } => write!(
                f,
                "session runner for backend `{}` is not implemented yet: {reason}",
                backend.as_str()
            ),
            Self::Action(err) => write!(f, "{err}"),
            Self::Failed { backend, message } => {
                write!(
                    f,
                    "session runner for backend `{}` failed: {message}",
                    backend.as_str()
                )
            }
        }
    }
}

impl Error for SessionRunnerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BackendUnavailable(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
            Self::Action(err) => Some(err),
            Self::Io(_) | Self::UnsupportedBackend { .. } | Self::Failed { .. } => None,
        }
    }
}

impl From<io::Error> for SessionRunnerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub struct SessionEventDispatcher<E> {
    executor: E,
}

impl<E> SessionEventDispatcher<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: SessionActionExecutor> SessionEventDispatcher<E> {
    pub fn dispatch_event<W: Write>(
        &mut self,
        writer: &mut W,
        event: SessionEvent,
    ) -> Result<(), SessionRunnerError> {
        self.dispatch_event_from_source(writer, EventSource::DesktopSession, event)
    }

    fn dispatch_event_from_source<W: Write>(
        &mut self,
        writer: &mut W,
        source: EventSource,
        event: SessionEvent,
    ) -> Result<(), SessionRunnerError> {
        match event {
            SessionEvent::Idle => {
                writeln!(writer, "LG Buddy Monitor: Session became idle.")?;
                let runtime_event = RuntimeEvent::from_session_event(source, event);
                match self.executor.screen_off(runtime_event) {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => {
                        writeln!(writer, "LG Buddy Monitor: screen-off action failed. {err}")?
                    }
                }
            }
            SessionEvent::Lock => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session locked; requesting screen blank."
                )?;
                let runtime_event = RuntimeEvent::from_session_event(source, event);
                match self.executor.screen_off(runtime_event) {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => {
                        writeln!(writer, "LG Buddy Monitor: screen-off action failed. {err}")?
                    }
                }
            }
            SessionEvent::Active | SessionEvent::WakeRequested | SessionEvent::UserActivity => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `{}` requests screen restore.",
                    event.as_str()
                )?;
                let runtime_event = RuntimeEvent::from_session_event(source, event);
                match self.executor.screen_on(runtime_event) {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => writeln!(
                        writer,
                        "LG Buddy Monitor: screen restore action failed. {err}"
                    )?,
                }
            }
            SessionEvent::BeforeSleep => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `before-sleep` requests pre-sleep handling."
                )?;
                let runtime_event = RuntimeEvent::from_session_event(source, event);
                match self.executor.before_sleep(runtime_event) {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => {
                        writeln!(writer, "LG Buddy Monitor: pre-sleep action failed. {err}")?
                    }
                }
            }
            SessionEvent::AfterResume => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `after-resume` requests wake restore."
                )?;
                writer.flush()?;
                let runtime_event = RuntimeEvent::from_session_event(source, event);
                match self.executor.after_resume_streaming(writer, runtime_event) {
                    Ok(()) => {}
                    Err(err) => writeln!(
                        writer,
                        "LG Buddy Monitor: wake restore action failed. {err}"
                    )?,
                }
            }
            SessionEvent::Unlock => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session unlocked; no screen restore requested."
                )?;
            }
        }

        Ok(())
    }

    fn dispatch_lifecycle_event<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<(), SessionRunnerError>
    where
        E: SessionActionExecutor,
    {
        match LifecycleEvent::from_runtime_event(event) {
            Some(LifecycleEvent::MachineResumed) => {
                writeln!(writer, "LG Buddy Lifecycle: System resumed from sleep.")?;
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `after-resume` requests wake restore."
                )?;
                writer.flush()?;
                match self.executor.after_resume_streaming(writer, event) {
                    Ok(()) => {}
                    Err(err) => writeln!(
                        writer,
                        "LG Buddy Lifecycle: wake restore action failed. {err}"
                    )?,
                }
            }
            Some(LifecycleEvent::MachinePreparingForSleep) => {
                writeln!(writer, "LG Buddy Lifecycle: System is preparing for sleep.")?;
                writeln!(
                    writer,
                    "LG Buddy Lifecycle: logind pre-sleep requests TV power-off before suspend."
                )?;
                writer.flush()?;
                match self.executor.before_sleep(event) {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => {
                        writeln!(writer, "LG Buddy Lifecycle: pre-sleep action failed. {err}")?
                    }
                }
            }
            Some(_) | None => {}
        }

        Ok(())
    }
}

pub fn run_monitor<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_monitor_with_executor(writer, RuntimeActionExecutor).map_err(|err| match err {
        SessionRunnerError::BackendSelection(err) => RunError::BackendSelection(err),
        SessionRunnerError::BackendDetection(err) => RunError::BackendDetection(err),
        other => RunError::Policy(other.to_string()),
    })
}

pub fn run_lifecycle_monitor<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    run_lifecycle_monitor_with_executor(writer, RuntimeActionExecutor, &config_path).map_err(
        |err| match err {
            SessionRunnerError::BackendSelection(err) => RunError::BackendSelection(err),
            SessionRunnerError::BackendDetection(err) => RunError::BackendDetection(err),
            other => RunError::Policy(other.to_string()),
        },
    )
}

fn run_lifecycle_monitor_with_executor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    executor: E,
    config_path: &Path,
) -> Result<(), SessionRunnerError> {
    let mut bus = new_system_bus_client().map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Auto,
        message: format!("failed to open system bus client: {err}"),
    })?;
    run_lifecycle_monitor_with_bus(writer, executor, config_path, &mut bus)
}

fn run_lifecycle_monitor_with_bus<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    executor: E,
    config_path: &Path,
    bus: &mut impl SessionBusClient,
) -> Result<(), SessionRunnerError> {
    add_logind_signal_match(bus).map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Auto,
        message: format!("failed to subscribe to logind lifecycle signals: {err}"),
    })?;
    let mut dispatcher = SessionEventDispatcher::new(executor);

    writeln!(
        writer,
        "LG Buddy Lifecycle: Using logind system lifecycle source."
    )?;

    let mut sleep_inhibitor = None;
    let mut waiting_for_resume = false;
    sync_lifecycle_sleep_delay_inhibitor_if_idle(
        writer,
        bus,
        config_path,
        &mut sleep_inhibitor,
        waiting_for_resume,
    )?;

    let started = Instant::now();
    let test_timeout = resolve_lifecycle_monitor_test_timeout();
    let test_event_limit = resolve_lifecycle_monitor_test_event_limit();
    let mut lifecycle_events_seen = 0usize;

    loop {
        if let Some(timeout) = test_timeout {
            if started.elapsed() >= timeout {
                return Ok(());
            }
        }

        let mut process_timeout = LOGIND_LIFECYCLE_PROCESS_INTERVAL;
        if let Some(timeout) = test_timeout {
            process_timeout = process_timeout
                .min(LOGIND_LIFECYCLE_TEST_PROCESS_INTERVAL)
                .min(timeout.saturating_sub(started.elapsed()));
        }

        sync_lifecycle_sleep_delay_inhibitor_if_idle(
            writer,
            bus,
            config_path,
            &mut sleep_inhibitor,
            waiting_for_resume,
        )?;

        let Some(signal) =
            bus.process(process_timeout)
                .map_err(|err| SessionRunnerError::Failed {
                    backend: ScreenBackend::Auto,
                    message: format!("logind lifecycle bus processing failed: {err}"),
                })?
        else {
            continue;
        };

        let Some(event) = map_prepare_for_sleep_signal(&signal) else {
            continue;
        };

        let event_kind = LifecycleEvent::from_runtime_event(event);
        match event_kind {
            Some(LifecycleEvent::MachinePreparingForSleep) => waiting_for_resume = true,
            Some(LifecycleEvent::MachineResumed) => waiting_for_resume = false,
            _ => {}
        }

        if !lifecycle_policy_enabled_from_config(config_path)? {
            release_lifecycle_sleep_delay_inhibitor(writer, &mut sleep_inhibitor)?;
            writeln!(
                writer,
                "LG Buddy Lifecycle: system sleep/wake handling is disabled by config; skipping lifecycle event."
            )?;
            lifecycle_events_seen += 1;
            if test_event_limit.is_some_and(|limit| lifecycle_events_seen >= limit) {
                return Ok(());
            }
            continue;
        }

        match event_kind {
            Some(LifecycleEvent::MachineResumed) => {
                dispatcher.dispatch_lifecycle_event(writer, event)?;
                sync_lifecycle_sleep_delay_inhibitor_if_idle(
                    writer,
                    bus,
                    config_path,
                    &mut sleep_inhibitor,
                    waiting_for_resume,
                )?;
            }
            Some(LifecycleEvent::MachinePreparingForSleep) => {
                let dispatch_result = dispatcher.dispatch_lifecycle_event(writer, event);
                let release_result =
                    release_lifecycle_sleep_delay_inhibitor(writer, &mut sleep_inhibitor);
                dispatch_result?;
                release_result?;
            }
            Some(_) => {
                dispatcher.dispatch_lifecycle_event(writer, event)?;
            }
            None => {}
        }

        lifecycle_events_seen += 1;
        if test_event_limit.is_some_and(|limit| lifecycle_events_seen >= limit) {
            return Ok(());
        }
    }
}

fn sync_lifecycle_sleep_delay_inhibitor_if_idle<W: Write>(
    writer: &mut W,
    bus: &mut impl SessionBusClient,
    config_path: &Path,
    sleep_inhibitor: &mut Option<OwnedFd>,
    waiting_for_resume: bool,
) -> Result<(), SessionRunnerError> {
    if !lifecycle_policy_enabled_from_config(config_path)? {
        release_lifecycle_sleep_delay_inhibitor(writer, sleep_inhibitor)?;
        return Ok(());
    }

    if waiting_for_resume || sleep_inhibitor.is_some() {
        return Ok(());
    }

    let inhibitor =
        acquire_sleep_delay_inhibitor(bus).map_err(|err| SessionRunnerError::Failed {
            backend: ScreenBackend::Auto,
            message: format!("failed to acquire logind sleep delay inhibitor: {err}"),
        })?;
    *sleep_inhibitor = Some(inhibitor);
    writeln!(
        writer,
        "LG Buddy Lifecycle: Acquired logind sleep delay inhibitor."
    )?;
    Ok(())
}

fn release_lifecycle_sleep_delay_inhibitor<W: Write>(
    writer: &mut W,
    sleep_inhibitor: &mut Option<OwnedFd>,
) -> Result<(), SessionRunnerError> {
    if sleep_inhibitor.take().is_some() {
        writeln!(
            writer,
            "LG Buddy Lifecycle: Released logind sleep delay inhibitor."
        )?;
    }

    Ok(())
}

fn lifecycle_policy_enabled_from_config(config_path: &Path) -> Result<bool, SessionRunnerError> {
    let config = load_config(config_path).map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Auto,
        message: format!("failed to load lifecycle config: {err}"),
    })?;
    Ok(config.system_sleep_wake_policy.is_enabled())
}

fn prepare_monitor_backend(
    probe: &mut SystemBackendProbe,
    configured: ScreenBackend,
) -> Result<(BackendResolution, Option<wayland_client::Connection>), BackendDetectionError> {
    let resolution = resolve_backend_with_probe(probe, configured)?;
    let connection = probe.take_wayland_connection();
    Ok((resolution, connection))
}

fn run_monitor_with_executor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    executor: E,
) -> Result<(), SessionRunnerError> {
    let screen_idle_blank_enabled = screen_idle_blank_enabled_from_config()?;
    if !screen_idle_blank_enabled {
        let _session_service = match spawn_session_notification_service() {
            Ok(service) => Some(service),
            Err(err) => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: session notification service unavailable: {err}"
                )?;
                None
            }
        };
        writeln!(
            writer,
            "LG Buddy Monitor: screen idle blanking is disabled by config."
        )?;
        return run_passive_session_agent(writer);
    }

    let initial_configured =
        configured_backend_from_env_or_config().map_err(SessionRunnerError::BackendSelection)?;
    let mut executor = Some(executor);
    let started = Instant::now();
    let test_timeout = resolve_gnome_monitor_test_timeout();
    let mut probe = SystemBackendProbe::default();
    let initial_resolution = prepare_monitor_backend(&mut probe, initial_configured);
    let mut initial_attempt = Some((initial_configured, initial_resolution));

    // Native probing must consume an inherited WAYLAND_SOCKET before this
    // thread starts. The same probe is retained for every later retry so the
    // automatic fallback policy cannot forget that one-shot socket state.
    let _session_service = match spawn_session_notification_service() {
        Ok(service) => Some(service),
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Monitor: session notification service unavailable: {err}"
            )?;
            None
        }
    };

    loop {
        let (configured, resolution) = match initial_attempt.take() {
            Some(attempt) => attempt,
            None => {
                if test_timeout_reached(started, test_timeout) {
                    return Ok(());
                }
                let configured = configured_backend_from_env_or_config()
                    .map_err(SessionRunnerError::BackendSelection)?;
                let resolution = prepare_monitor_backend(&mut probe, configured);
                (configured, resolution)
            }
        };

        match resolution {
            Ok((resolution, mut wayland_connection)) => {
                if configured == ScreenBackend::Auto {
                    writeln!(
                        writer,
                        "LG Buddy Monitor: auto resolved to {}.",
                        resolution.backend().as_str()
                    )?;
                    if let Some(reason) = resolution.fallback_reason() {
                        writeln!(writer, "LG Buddy Monitor: fallback reason: {reason}")?;
                    }
                }
                if resolution.backend() == ScreenBackend::Swayidle {
                    writeln!(
                        writer,
                        "LG Buddy Monitor: warning: {SWAYIDLE_DEPRECATION_NOTICE}."
                    )?;
                }

                match resolution.backend() {
                    ScreenBackend::Gnome => {
                        let mut dispatcher = SessionEventDispatcher::new(
                            executor.take().expect("executor available"),
                        );
                        return run_gnome_monitor(writer, &mut dispatcher);
                    }
                    ScreenBackend::Wayland => {
                        let mut dispatcher = SessionEventDispatcher::new(
                            executor.take().expect("executor available"),
                        );
                        let connection = wayland_connection.take().ok_or_else(|| {
                            SessionRunnerError::Failed {
                                backend: ScreenBackend::Wayland,
                                message: "native Wayland was selected without retaining its verified connection"
                                    .to_string(),
                            }
                        })?;
                        return run_wayland_monitor(writer, &mut dispatcher, connection);
                    }
                    ScreenBackend::Swayidle => return run_swayidle_monitor(writer),
                    ScreenBackend::Auto => {
                        return Err(SessionRunnerError::Failed {
                            backend: ScreenBackend::Auto,
                            message: "auto backend should be resolved before starting the runner"
                                .to_string(),
                        });
                    }
                }
            }
            Err(err) => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: screen idle backend unavailable: {err}"
                )?;
                wait_for_backend_retry_or_test_timeout(started, test_timeout);
            }
        }
    }
}

fn screen_idle_blank_enabled_from_config() -> Result<bool, SessionRunnerError> {
    let config_path = match resolve_config_path_from_env() {
        Ok(path) => path,
        Err(ConfigPathError::NotConfigured) => return Ok(true),
    };
    let contents = fs::read_to_string(&config_path).map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Auto,
        message: format!(
            "failed to load screen idle blank config from {}: {err}",
            config_path.display()
        ),
    })?;
    let entries = parse_config_entries(&contents);
    let policy = entries
        .get("screen_idle_blank")
        .and_then(|value| value.parse::<ScreenIdleBlankPolicy>().ok())
        .unwrap_or(ScreenIdleBlankPolicy::Enabled);

    Ok(policy.is_enabled())
}

fn run_passive_session_agent<W: Write>(_writer: &mut W) -> Result<(), SessionRunnerError> {
    let started = Instant::now();
    let test_timeout = resolve_gnome_monitor_test_timeout();

    loop {
        if test_timeout_reached(started, test_timeout) {
            return Ok(());
        }

        thread::sleep(passive_sleep_duration(started, test_timeout));
    }
}

fn wait_for_backend_retry_or_test_timeout(started: Instant, test_timeout: Option<Duration>) {
    if test_timeout_reached(started, test_timeout) {
        return;
    }

    thread::sleep(passive_sleep_duration(started, test_timeout));
}

fn passive_sleep_duration(started: Instant, test_timeout: Option<Duration>) -> Duration {
    let mut sleep_for = SESSION_AGENT_BACKEND_RETRY_INTERVAL;
    if let Some(timeout) = test_timeout {
        sleep_for = sleep_for.min(timeout.saturating_sub(started.elapsed()));
    }

    if sleep_for.is_zero() {
        Duration::from_millis(1)
    } else {
        sleep_for
    }
}

fn test_timeout_reached(started: Instant, test_timeout: Option<Duration>) -> bool {
    test_timeout.is_some_and(|timeout| started.elapsed() >= timeout)
}

fn run_gnome_monitor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
) -> Result<(), SessionRunnerError> {
    wait_for_gnome_shell()?;

    GnomeBackend::new(SystemGnomeProbe)
        .capabilities()
        .map_err(SessionRunnerError::BackendUnavailable)?;

    writeln!(writer, "LG Buddy Monitor: Using GNOME backend.")?;

    run_native_session_monitor(
        writer,
        dispatcher,
        ScreenBackend::Gnome,
        spawn_gnome_monitor_thread,
    )
}

fn run_wayland_monitor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    connection: wayland_client::Connection,
) -> Result<(), SessionRunnerError> {
    writeln!(writer, "LG Buddy Monitor: Using native Wayland backend.")?;

    run_native_session_monitor(
        writer,
        dispatcher,
        ScreenBackend::Wayland,
        move |sender, latest_observation| {
            spawn_wayland_monitor_thread(connection, sender, latest_observation)
        },
    )
}

fn run_native_session_monitor<W, E, S>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    backend: ScreenBackend,
    spawn_monitor: S,
) -> Result<(), SessionRunnerError>
where
    W: Write,
    E: SessionActionExecutor,
    S: FnOnce(mpsc::Sender<RunnerMessage>, Arc<LatestInactivityObservation>) -> JoinHandle<()>,
{
    run_native_session_monitor_with_lock_monitor(
        writer,
        dispatcher,
        backend,
        spawn_monitor,
        |sender| Some(spawn_logind_lock_thread(sender)),
    )
}

fn run_native_session_monitor_with_lock_monitor<W, E, S, L>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    backend: ScreenBackend,
    spawn_monitor: S,
    spawn_lock_monitor: L,
) -> Result<(), SessionRunnerError>
where
    W: Write,
    E: SessionActionExecutor,
    S: FnOnce(mpsc::Sender<RunnerMessage>, Arc<LatestInactivityObservation>) -> JoinHandle<()>,
    L: FnOnce(mpsc::Sender<RunnerMessage>) -> Option<LogindLockThread>,
{
    let blank_after = Duration::from_millis(resolve_idle_timeout_ms());
    let started_at = Instant::now();
    let mut inactivity = if session_screen_ownership_marker_exists(backend)? {
        InactivityEngine::new_with_restore_pending(blank_after, started_at)
    } else {
        InactivityEngine::new(blank_after, started_at)
    };

    let (sender, receiver) = mpsc::channel();
    let latest_inactivity = Arc::new(LatestInactivityObservation::default());
    let monitor_handle = spawn_monitor(sender.clone(), Arc::clone(&latest_inactivity));
    let _gamepad_monitor = spawn_gamepad_activity_thread(sender.clone());
    let _logind_lock_monitor = spawn_lock_monitor(sender.clone());
    let mut monitor_result = Ok(());

    loop {
        let message = match inactivity.time_until_blank(Instant::now()) {
            Some(wait) => match receiver.recv_timeout(wait) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    handle_inactivity_timeout(writer, dispatcher, &mut inactivity, Instant::now())?;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };

        match message {
            RunnerMessage::InactivityObservationReady => {
                if let Some(observation) = latest_inactivity.take() {
                    handle_inactivity_observation(
                        writer,
                        dispatcher,
                        &mut inactivity,
                        EventSource::DesktopSession,
                        observation.observation,
                        observation.observed_at,
                    )?;
                }
            }
            RunnerMessage::ActivityObservation {
                source,
                observation,
                observed_at,
            } => handle_inactivity_observation(
                writer,
                dispatcher,
                &mut inactivity,
                source,
                observation,
                observed_at,
            )?,
            RunnerMessage::SessionEvent {
                event: SessionEvent::Idle,
                ..
            } => {
                // Provider idle is observational. Only LG Buddy's inactivity
                // deadline decides when to blank.
            }
            RunnerMessage::SessionEvent {
                event: SessionEvent::Active,
                source,
                observed_at,
            } => handle_inactivity_observation(
                writer,
                dispatcher,
                &mut inactivity,
                source,
                InactivityObservation::ProviderActive,
                observed_at,
            )?,
            RunnerMessage::SessionEvent {
                event: SessionEvent::WakeRequested,
                source,
                observed_at,
            } => handle_inactivity_observation(
                writer,
                dispatcher,
                &mut inactivity,
                source,
                InactivityObservation::WakeRequested,
                observed_at,
            )?,
            RunnerMessage::SessionEvent {
                event: SessionEvent::UserActivity,
                source,
                observed_at,
            } => handle_inactivity_observation(
                writer,
                dispatcher,
                &mut inactivity,
                source,
                InactivityObservation::UserActivityObserved,
                observed_at,
            )?,
            RunnerMessage::SessionEvent {
                event: SessionEvent::Lock,
                source,
                observed_at,
            } => handle_session_lock(writer, dispatcher, &mut inactivity, source, observed_at)?,
            RunnerMessage::SessionEvent {
                event: SessionEvent::Unlock,
                source,
                ..
            } => {
                inactivity.observe_unlock();
                dispatcher.dispatch_event_from_source(writer, source, SessionEvent::Unlock)?;
            }
            RunnerMessage::SessionEvent { event, source, .. } => {
                dispatcher.dispatch_event_from_source(writer, source, event)?;
            }
            RunnerMessage::Diagnostic(message) => {
                writeln!(writer, "LG Buddy Monitor: {message}")?;
            }
            RunnerMessage::MonitorExited(result) => {
                monitor_result = result;
                break;
            }
        }
    }

    let _ = monitor_handle.join();
    monitor_result
}

fn run_swayidle_monitor<W: Write>(writer: &mut W) -> Result<(), SessionRunnerError> {
    let idle_timeout_secs = resolve_idle_timeout_secs();
    let current_exe = std::env::current_exe()?;
    let screen_off_command = format!("{} screen off", shell_quote(&current_exe));
    let screen_on_command = format!("{} screen on", shell_quote(&current_exe));

    writeln!(
        writer,
        "LG Buddy Monitor: Using swayidle backend (timeout: {idle_timeout_secs}s)."
    )?;

    let status = ProcessCommand::new("swayidle")
        .args([
            "-w",
            "timeout",
            &idle_timeout_secs.to_string(),
            &screen_off_command,
            "resume",
            &screen_on_command,
        ])
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(SessionRunnerError::Failed {
            backend: ScreenBackend::Swayidle,
            message: format!("swayidle exited with status {status}"),
        })
    }
}

fn wait_for_gnome_shell() -> Result<(), SessionRunnerError> {
    let mut bus = new_session_bus_client().map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to open GNOME session bus client: {err}"),
    })?;
    bus.wait_for_name(
        GNOME_SHELL_NAME,
        Duration::from_secs(GNOME_WAIT_TIMEOUT_SECS),
    )
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed waiting for GNOME Shell on the session bus: {err}"),
    })
}

fn resolve_idle_timeout_secs() -> u64 {
    normalize_idle_timeout_secs(
        std::env::var("LG_BUDDY_IDLE_TIMEOUT")
            .ok()
            .and_then(|value| parse_idle_timeout_secs(&value))
            .or_else(|| {
                let path = resolve_config_path_from_env().ok()?;
                load_config(&path)
                    .ok()
                    .map(|config| config.screen_idle_timeout)
            })
            .map(u128::from)
            .unwrap_or(u128::from(DEFAULT_IDLE_TIMEOUT)),
    )
}

fn session_screen_ownership_marker_exists(
    backend: ScreenBackend,
) -> Result<bool, SessionRunnerError> {
    ScreenOwnershipMarker::from_env(StateScope::Session)
        .map(|marker| marker.exists())
        .map_err(|err| SessionRunnerError::Failed {
            backend,
            message: format!("failed to inspect session screen ownership: {err}"),
        })
}

fn resolve_idle_timeout_ms() -> u64 {
    resolve_idle_timeout_secs().saturating_mul(1000)
}

fn resolve_gnome_monitor_test_timeout() -> Option<Duration> {
    std::env::var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
}

fn resolve_lifecycle_monitor_test_timeout() -> Option<Duration> {
    std::env::var(LIFECYCLE_MONITOR_TEST_TIMEOUT_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
}

fn resolve_lifecycle_monitor_test_event_limit() -> Option<usize> {
    std::env::var(LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn spawn_gnome_monitor_thread(
    sender: mpsc::Sender<RunnerMessage>,
    latest_observation: Arc<LatestInactivityObservation>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = run_gnome_monitor_process(&sender, &latest_observation);
        let _ = sender.send(RunnerMessage::MonitorExited(result));
    })
}

fn spawn_wayland_monitor_thread(
    connection: wayland_client::Connection,
    sender: mpsc::Sender<RunnerMessage>,
    latest_observation: Arc<LatestInactivityObservation>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let activity_sender = sender.clone();
        let result = run_wayland_activity_monitor(connection, move |observed_at| {
            latest_observation.publish(
                &activity_sender,
                InactivityObservation::DesktopActivityObserved,
                observed_at,
            )
        })
        .map_err(|err| SessionRunnerError::Failed {
            backend: ScreenBackend::Wayland,
            message: err.to_string(),
        });
        let _ = sender.send(RunnerMessage::MonitorExited(result));
    })
}

fn spawn_logind_lock_thread(sender: mpsc::Sender<RunnerMessage>) -> LogindLockThread {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let result = new_system_bus_client()
            .map_err(Into::into)
            .and_then(|mut bus| run_logind_lock_monitor_process(&mut bus, &sender, &thread_stop));
        report_logind_lock_monitor_result(&sender, result);
    });

    LogindLockThread {
        stop,
        handle: Some(handle),
    }
}

fn report_logind_lock_monitor_result(
    sender: &mpsc::Sender<RunnerMessage>,
    result: Result<(), crate::sources::linux::logind::LogindSessionError>,
) {
    if let Err(err) = result {
        let _ = sender.send(RunnerMessage::Diagnostic(format!(
            "session lock monitoring unavailable: {err}"
        )));
    }
}

fn run_logind_lock_monitor_process(
    bus: &mut impl SessionBusClient,
    sender: &mpsc::Sender<RunnerMessage>,
    stop: &AtomicBool,
) -> Result<(), LogindSessionError> {
    let explicit_session_id = std::env::var("XDG_SESSION_ID").ok();
    let current_uid = current_effective_uid();
    add_logind_owner_signal_match(bus)?;
    let (mut session, owner, initial_locked) =
        bind_logind_lock_target(bus, explicit_session_id.as_deref(), current_uid)?;
    let mut logind_owner = Some(owner);

    let mut tracker = LockedHintTracker::default();
    if !send_locked_hint_event(sender, &mut tracker, initial_locked) {
        return Ok(());
    }

    while !stop.load(Ordering::SeqCst) {
        let Some(signal) = bus.process(LOGIND_LOCK_PROCESS_INTERVAL)? else {
            continue;
        };

        if let Some(new_owner) = logind_owner_changed(&signal) {
            if new_owner.is_none() {
                logind_owner = None;
                continue;
            }

            let (new_session, owner, locked) =
                bind_logind_lock_target(bus, explicit_session_id.as_deref(), current_uid)?;
            session = new_session;
            logind_owner = Some(owner);
            if !send_locked_hint_event(sender, &mut tracker, locked) {
                return Ok(());
            }
            continue;
        }

        let Some(logind_owner) = logind_owner.as_deref() else {
            continue;
        };
        let Some(change) = map_locked_hint_change(&signal, &session, logind_owner) else {
            continue;
        };
        let locked = match change {
            LockedHintChange::Changed(locked) => locked,
            LockedHintChange::Invalidated => locked_hint(bus, &session)?,
        };
        if !send_locked_hint_event(sender, &mut tracker, locked) {
            return Ok(());
        }
    }

    Ok(())
}

fn bind_logind_lock_target(
    bus: &mut impl SessionBusClient,
    explicit_session_id: Option<&str>,
    current_uid: u32,
) -> Result<(LogindSession, String, bool), LogindSessionError> {
    let session = resolve_current_graphical_session(bus, explicit_session_id, current_uid)?;
    let owner = resolve_logind_owner(bus)?;
    add_locked_hint_signal_match(bus, &session, &owner)?;
    let locked = locked_hint(bus, &session)?;
    Ok((session, owner, locked))
}

fn send_locked_hint_event(
    sender: &mpsc::Sender<RunnerMessage>,
    tracker: &mut LockedHintTracker,
    locked: bool,
) -> bool {
    let Some(event) = tracker.observe(locked) else {
        return true;
    };

    sender
        .send(RunnerMessage::SessionEvent {
            event,
            source: EventSource::LinuxLogind,
            observed_at: Instant::now(),
        })
        .is_ok()
}

fn current_effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    unsafe { libc::geteuid() }
}

fn spawn_gamepad_activity_thread(
    sender: mpsc::Sender<RunnerMessage>,
) -> Option<GamepadActivityThread> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = match resolve_gamepad_activity_source_mode() {
        GamepadActivitySourceMode::Disabled => return None,
        GamepadActivitySourceMode::Synthetic(delay) => thread::spawn(move || {
            run_synthetic_gamepad_activity_process(sender, thread_stop, delay)
        }),
        GamepadActivitySourceMode::System => {
            thread::spawn(move || run_gamepad_activity_process(sender, thread_stop))
        }
    };

    Some(GamepadActivityThread {
        stop,
        handle: Some(handle),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamepadActivitySourceMode {
    System,
    Synthetic(Duration),
    Disabled,
}

fn resolve_gamepad_activity_source_mode() -> GamepadActivitySourceMode {
    match std::env::var(GAMEPAD_ACTIVITY_SOURCE_ENV).ok().as_deref() {
        Some("disabled" | "none" | "off") => GamepadActivitySourceMode::Disabled,
        Some("synthetic" | "test") => GamepadActivitySourceMode::Synthetic(
            resolve_gamepad_activity_test_delay().unwrap_or(Duration::ZERO),
        ),
        Some("system" | "real") => GamepadActivitySourceMode::System,
        Some(_) | None => resolve_gamepad_activity_test_delay()
            .map(GamepadActivitySourceMode::Synthetic)
            .unwrap_or(GamepadActivitySourceMode::System),
    }
}

fn resolve_gamepad_activity_test_delay() -> Option<Duration> {
    std::env::var(GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
}

fn run_synthetic_gamepad_activity_process(
    sender: mpsc::Sender<RunnerMessage>,
    stop: Arc<AtomicBool>,
    delay: Duration,
) {
    let started = Instant::now();
    while started.elapsed() < delay {
        if stop.load(Ordering::SeqCst) {
            return;
        }

        thread::sleep(
            delay
                .saturating_sub(started.elapsed())
                .min(GAMEPAD_ACTIVITY_POLL_INTERVAL),
        );
    }

    if !stop.load(Ordering::SeqCst) {
        let _ = sender.send(RunnerMessage::ActivityObservation {
            source: EventSource::AuxiliaryInput,
            observation: InactivityObservation::UserActivityObserved,
            observed_at: Instant::now(),
        });
    }
}

fn run_gamepad_activity_process(sender: mpsc::Sender<RunnerMessage>, stop: Arc<AtomicBool>) {
    let mut diagnostics = GamepadDiagnosticEmitter::default();
    let mut event_monitor = match open_system_gamepad_device_event_monitor() {
        Ok(monitor) => Some(monitor),
        Err(err) => {
            if !diagnostics.send_all(
                &sender,
                vec![format!(
                    "gamepad device event monitor unavailable: {err}; falling back to periodic reconciliation"
                )],
            ) {
                return;
            }
            None
        }
    };
    let mut source: Option<SystemGamepadActivitySource> = None;
    let mut pending_refresh_at = Some(Instant::now());
    let mut next_reconcile_at = Instant::now() + GAMEPAD_ACTIVITY_RECONCILE_INTERVAL;
    let mut last_activity_sent_at = None;

    while !stop.load(Ordering::SeqCst) {
        let observed_at = Instant::now();

        match gamepad_device_event_refresh_requested(&sender, &mut diagnostics, &mut event_monitor)
        {
            GamepadDeviceEventRefresh::Requested => {
                schedule_gamepad_refresh(
                    &mut pending_refresh_at,
                    observed_at + GAMEPAD_ACTIVITY_REFRESH_DEBOUNCE,
                );
            }
            GamepadDeviceEventRefresh::NotRequested => {}
            GamepadDeviceEventRefresh::Stop => return,
        }

        if observed_at >= next_reconcile_at {
            schedule_gamepad_refresh(&mut pending_refresh_at, observed_at);
            next_reconcile_at = observed_at + GAMEPAD_ACTIVITY_RECONCILE_INTERVAL;
        }

        if gamepad_refresh_due(pending_refresh_at, observed_at) {
            let mut retry_refresh = false;

            if let Some(current_source) = source.as_mut() {
                let refresh = current_source.refresh(observed_at);
                retry_refresh |= refresh.retry_requested;
                if !diagnostics.send_all(&sender, refresh.diagnostics) {
                    return;
                }
                if current_source.is_empty() {
                    source = None;
                }
            }

            if source.is_none() {
                let setup = open_system_gamepad_activity_source();
                if !diagnostics.send_all(&sender, setup.diagnostics) {
                    return;
                }
                retry_refresh |= setup.retry_requested;
                source = setup.source;
            }

            complete_gamepad_refresh(&mut pending_refresh_at, observed_at, retry_refresh);
        }

        let Some(current_source) = source.as_mut() else {
            thread::sleep(GAMEPAD_ACTIVITY_POLL_INTERVAL);
            continue;
        };

        let poll = current_source.poll_once(observed_at);
        if !diagnostics.send_all(&sender, poll.diagnostics) {
            return;
        }

        if current_source.is_empty() {
            source = None;
            schedule_gamepad_refresh(&mut pending_refresh_at, Instant::now());
        }

        if poll.activity && gamepad_activity_send_due(last_activity_sent_at, observed_at) {
            if sender
                .send(RunnerMessage::ActivityObservation {
                    source: EventSource::AuxiliaryInput,
                    observation: InactivityObservation::UserActivityObserved,
                    observed_at,
                })
                .is_err()
            {
                return;
            }
            last_activity_sent_at = Some(observed_at);
        }

        thread::sleep(GAMEPAD_ACTIVITY_POLL_INTERVAL);
    }
}

fn run_gnome_monitor_process(
    sender: &mpsc::Sender<RunnerMessage>,
    latest_observation: &LatestInactivityObservation,
) -> Result<(), SessionRunnerError> {
    let mut bus = new_session_bus_client().map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to open GNOME session bus client: {err}"),
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: None,
        path: Some(GNOME_SCREEN_SAVER_PATH),
        interface: Some(GNOME_SCREEN_SAVER_INTERFACE),
        member: None,
    })
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to subscribe to GNOME ScreenSaver signals: {err}"),
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: Some(DBUS_SERVICE_NAME),
        path: Some(DBUS_OBJECT_PATH),
        interface: Some(DBUS_INTERFACE),
        member: Some("NameOwnerChanged"),
    })
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to subscribe to D-Bus owner changes: {err}"),
    })?;
    let mut trusted_screen_saver_signals = TrustedScreenSaverSignals::new(Some(
        resolve_screen_saver_owner(&mut bus).map_err(|err| SessionRunnerError::Failed {
            backend: ScreenBackend::Gnome,
            message: format!("failed to resolve GNOME ScreenSaver owner: {err}"),
        })?,
    ));

    let started = Instant::now();
    let test_timeout = resolve_gnome_monitor_test_timeout();
    let mut next_idle_poll = Instant::now();

    loop {
        if let Some(timeout) = test_timeout {
            if started.elapsed() >= timeout {
                return Ok(());
            }
        }

        let now = Instant::now();
        if now >= next_idle_poll {
            if !poll_gnome_idle_monitor_once(&mut bus, sender, latest_observation) {
                return Ok(());
            }
            next_idle_poll = now + GNOME_IDLE_POLL_INTERVAL;
        }

        let now = Instant::now();
        let mut process_timeout = next_idle_poll
            .saturating_duration_since(now)
            .min(GNOME_BUS_PROCESS_INTERVAL);
        if let Some(timeout) = test_timeout {
            process_timeout = process_timeout.min(timeout.saturating_sub(started.elapsed()));
        }

        let Some(signal) =
            bus.process(process_timeout)
                .map_err(|err| SessionRunnerError::Failed {
                    backend: ScreenBackend::Gnome,
                    message: format!("GNOME session bus processing failed: {err}"),
                })?
        else {
            continue;
        };

        let Some(event) = trusted_screen_saver_signals.observe(&signal) else {
            continue;
        };

        if sender
            .send(RunnerMessage::SessionEvent {
                event,
                source: EventSource::DesktopSession,
                observed_at: Instant::now(),
            })
            .is_err()
        {
            return Ok(());
        }
    }
}

enum RunnerMessage {
    SessionEvent {
        event: SessionEvent,
        source: EventSource,
        observed_at: Instant,
    },
    ActivityObservation {
        source: EventSource,
        observation: InactivityObservation,
        observed_at: Instant,
    },
    InactivityObservationReady,
    Diagnostic(String),
    MonitorExited(Result<(), SessionRunnerError>),
}

#[derive(Debug)]
struct GamepadActivityThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct LogindLockThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LogindLockThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for GamepadActivityThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn gamepad_activity_send_due(last_sent_at: Option<Instant>, observed_at: Instant) -> bool {
    last_sent_at
        .map(|last_sent_at| {
            observed_at
                .checked_duration_since(last_sent_at)
                .unwrap_or_default()
                >= GAMEPAD_ACTIVITY_SEND_INTERVAL
        })
        .unwrap_or(true)
}

trait GamepadDeviceEventMonitor {
    fn has_relevant_event(&mut self) -> io::Result<bool>;
}

impl GamepadDeviceEventMonitor for SystemGamepadDeviceEventMonitor {
    fn has_relevant_event(&mut self) -> io::Result<bool> {
        SystemGamepadDeviceEventMonitor::has_relevant_event(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamepadDeviceEventRefresh {
    Requested,
    NotRequested,
    Stop,
}

fn gamepad_device_event_refresh_requested<M>(
    sender: &mpsc::Sender<RunnerMessage>,
    diagnostics: &mut GamepadDiagnosticEmitter,
    event_monitor: &mut Option<M>,
) -> GamepadDeviceEventRefresh
where
    M: GamepadDeviceEventMonitor,
{
    let Some(monitor) = event_monitor.as_mut() else {
        return GamepadDeviceEventRefresh::NotRequested;
    };

    match monitor.has_relevant_event() {
        Ok(true) => GamepadDeviceEventRefresh::Requested,
        Ok(false) => GamepadDeviceEventRefresh::NotRequested,
        Err(err) => {
            *event_monitor = None;
            if diagnostics.send_all(
                sender,
                vec![format!(
                    "gamepad device event monitor stopped: {err}; falling back to periodic reconciliation"
                )],
            ) {
                GamepadDeviceEventRefresh::NotRequested
            } else {
                GamepadDeviceEventRefresh::Stop
            }
        }
    }
}

fn schedule_gamepad_refresh(pending_refresh_at: &mut Option<Instant>, refresh_at: Instant) {
    if pending_refresh_at
        .map(|pending_refresh_at| pending_refresh_at <= refresh_at)
        .unwrap_or(false)
    {
        return;
    }

    *pending_refresh_at = Some(refresh_at);
}

fn gamepad_refresh_due(pending_refresh_at: Option<Instant>, observed_at: Instant) -> bool {
    pending_refresh_at
        .map(|pending_refresh_at| observed_at >= pending_refresh_at)
        .unwrap_or(false)
}

fn complete_gamepad_refresh(
    pending_refresh_at: &mut Option<Instant>,
    observed_at: Instant,
    retry_requested: bool,
) {
    *pending_refresh_at = None;

    if retry_requested {
        schedule_gamepad_refresh(
            pending_refresh_at,
            observed_at + GAMEPAD_ACTIVITY_REFRESH_RETRY_INTERVAL,
        );
    }
}

#[derive(Debug, Default)]
struct GamepadDiagnosticEmitter {
    seen: HashSet<String>,
}

impl GamepadDiagnosticEmitter {
    fn send_all(&mut self, sender: &mpsc::Sender<RunnerMessage>, diagnostics: Vec<String>) -> bool {
        for diagnostic in diagnostics {
            if self.seen.insert(diagnostic.clone())
                && sender.send(RunnerMessage::Diagnostic(diagnostic)).is_err()
            {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Default)]
struct LatestInactivityObservation {
    state: Mutex<LatestInactivityObservationState>,
}

#[derive(Debug, Default)]
struct LatestInactivityObservationState {
    observation: Option<TimedInactivityObservation>,
    notification_in_flight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedInactivityObservation {
    observation: InactivityObservation,
    observed_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedScreenSaverSignals {
    owner: Option<String>,
}

impl TrustedScreenSaverSignals {
    fn new(owner: Option<String>) -> Self {
        Self { owner }
    }

    fn observe(&mut self, signal: &BusSignal) -> Option<SessionEvent> {
        if signal.path == DBUS_OBJECT_PATH
            && signal.interface == DBUS_INTERFACE
            && signal.member == "NameOwnerChanged"
        {
            if signal.sender.as_deref() != Some(DBUS_SERVICE_NAME) {
                return None;
            }
            if let Some(new_owner) = screen_saver_owner_changed(signal) {
                self.owner = new_owner;
            }
            return None;
        }

        if signal.sender.as_deref() != self.owner.as_deref() {
            return None;
        }

        map_screen_saver_signal(signal)
    }
}

impl LatestInactivityObservation {
    fn publish(
        &self,
        sender: &mpsc::Sender<RunnerMessage>,
        observation: InactivityObservation,
        observed_at: Instant,
    ) -> bool {
        let should_notify = {
            let mut state = self
                .state
                .lock()
                .expect("latest inactivity observation lock");
            state.observation = Some(TimedInactivityObservation {
                observation,
                observed_at,
            });
            if state.notification_in_flight {
                false
            } else {
                state.notification_in_flight = true;
                true
            }
        };

        if !should_notify {
            return true;
        }

        if sender
            .send(RunnerMessage::InactivityObservationReady)
            .is_ok()
        {
            return true;
        }

        let mut state = self
            .state
            .lock()
            .expect("latest inactivity observation lock");
        state.notification_in_flight = false;
        false
    }

    fn take(&self) -> Option<TimedInactivityObservation> {
        let mut state = self
            .state
            .lock()
            .expect("latest inactivity observation lock");
        let observation = state.observation.take();
        state.notification_in_flight = false;
        observation
    }
}

fn poll_gnome_idle_monitor_once(
    bus: &mut impl SessionBusClient,
    sender: &mpsc::Sender<RunnerMessage>,
    latest_observation: &LatestInactivityObservation,
) -> bool {
    let Ok(idletime_ms) = current_idle_monitor_idletime_ms(bus) else {
        return true;
    };

    let activity_age = Duration::from_millis(idletime_ms);
    if activity_age > GNOME_DESKTOP_ACTIVITY_WINDOW {
        return true;
    }

    let observed_at = Instant::now();
    let activity_at = observed_at.checked_sub(activity_age).unwrap_or(observed_at);

    latest_observation.publish(
        sender,
        InactivityObservation::DesktopActivityObserved,
        activity_at,
    )
}

fn handle_inactivity_observation<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    inactivity: &mut InactivityEngine,
    source: EventSource,
    observation: InactivityObservation,
    observed_at: Instant,
) -> Result<(), SessionRunnerError> {
    let decision = inactivity.observe_activity(observation, observed_at);
    let event = match (observation, decision) {
        (_, InactivityDecision::NoOp) => None,
        (_, InactivityDecision::BlankNow) => None,
        (InactivityObservation::ProviderActive, InactivityDecision::RestoreNow) => {
            Some(SessionEvent::Active)
        }
        (InactivityObservation::WakeRequested, InactivityDecision::RestoreNow) => {
            Some(SessionEvent::WakeRequested)
        }
        (
            InactivityObservation::DesktopActivityObserved
            | InactivityObservation::UserActivityObserved,
            InactivityDecision::RestoreNow,
        ) => Some(SessionEvent::UserActivity),
    };

    if let Some(event) = event {
        dispatcher.dispatch_event_from_source(writer, source, event)?;
    }

    Ok(())
}

fn handle_inactivity_timeout<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    inactivity: &mut InactivityEngine,
    observed_at: Instant,
) -> Result<(), SessionRunnerError> {
    if inactivity.observe_time(observed_at) == InactivityDecision::BlankNow {
        dispatcher.dispatch_event(writer, SessionEvent::Idle)?;
    }

    Ok(())
}

fn handle_session_lock<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    inactivity: &mut InactivityEngine,
    source: EventSource,
    observed_at: Instant,
) -> Result<(), SessionRunnerError> {
    if inactivity.observe_lock(observed_at) == InactivityDecision::BlankNow {
        dispatcher.dispatch_event_from_source(writer, source, SessionEvent::Lock)?;
    }

    Ok(())
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let escaped = rendered.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn run_action<F>(action: F) -> Result<String, RunError>
where
    F: FnOnce(&mut Vec<u8>) -> Result<(), RunError>,
{
    let mut output = Vec::new();
    action(&mut output)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn write_command_output<W: Write>(writer: &mut W, output: &str) -> io::Result<()> {
    if output.is_empty() {
        return Ok(());
    }

    write!(writer, "{output}")?;
    if !output.ends_with('\n') {
        writeln!(writer)?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        complete_gamepad_refresh, gamepad_activity_send_due,
        gamepad_device_event_refresh_requested, gamepad_refresh_due, handle_inactivity_observation,
        handle_inactivity_timeout, normalize_idle_timeout_secs, poll_gnome_idle_monitor_once,
        report_logind_lock_monitor_result, run_lifecycle_monitor_with_bus,
        run_logind_lock_monitor_process, run_native_session_monitor_with_lock_monitor,
        schedule_gamepad_refresh, shell_quote, GamepadDeviceEventMonitor,
        GamepadDeviceEventRefresh, GamepadDiagnosticEmitter, LatestInactivityObservation,
        RunnerMessage, SessionActionExecutor, SessionEventDispatcher, TimedInactivityObservation,
        TrustedScreenSaverSignals, GAMEPAD_ACTIVITY_REFRESH_RETRY_INTERVAL,
        GAMEPAD_ACTIVITY_SEND_INTERVAL,
    };
    use crate::config::ScreenBackend;
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::session::inactivity::{InactivityEngine, InactivityObservation};
    use crate::session::SessionEvent;
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use crate::sources::desktop::gnome::{
        GNOME_SCREEN_SAVER_INTERFACE, GNOME_SCREEN_SAVER_NAME, GNOME_SCREEN_SAVER_PATH,
    };
    use crate::sources::linux::logind::{
        LogindSessionError, LOGIND_MANAGER_INTERFACE, LOGIND_MANAGER_PATH, LOGIND_SERVICE_NAME,
        LOGIND_SESSION_INTERFACE,
    };
    use crate::RunError;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{atomic::AtomicBool, mpsc, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Debug, Default)]
    struct FakeActionExecutor {
        screen_off_calls: usize,
        screen_on_calls: usize,
        screen_off_events: Vec<RuntimeEvent>,
        screen_on_events: Vec<RuntimeEvent>,
        before_sleep_calls: usize,
        after_resume_calls: usize,
        before_sleep_events: Vec<RuntimeEvent>,
        after_resume_events: Vec<RuntimeEvent>,
        screen_off_output: String,
        screen_on_output: String,
        before_sleep_output: String,
        after_resume_output: String,
        screen_off_error: Option<String>,
        screen_on_error: Option<String>,
        screen_on_signal: Option<mpsc::Sender<()>>,
        before_sleep_error: Option<String>,
        after_resume_error: Option<String>,
    }

    #[derive(Debug)]
    struct FakeGamepadDeviceEventMonitor {
        result: Option<io::Result<bool>>,
    }

    impl GamepadDeviceEventMonitor for FakeGamepadDeviceEventMonitor {
        fn has_relevant_event(&mut self) -> io::Result<bool> {
            self.result.take().expect("event monitor result")
        }
    }

    impl SessionActionExecutor for FakeActionExecutor {
        fn screen_off(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
            self.screen_off_calls += 1;
            self.screen_off_events.push(event);
            if let Some(message) = &self.screen_off_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.screen_off_output.clone())
        }

        fn screen_on(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
            self.screen_on_calls += 1;
            self.screen_on_events.push(event);
            if let Some(sender) = &self.screen_on_signal {
                let _ = sender.send(());
            }
            if let Some(message) = &self.screen_on_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.screen_on_output.clone())
        }

        fn before_sleep(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
            self.before_sleep_calls += 1;
            self.before_sleep_events.push(event);
            if let Some(message) = &self.before_sleep_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.before_sleep_output.clone())
        }

        fn after_resume(&mut self, event: RuntimeEvent) -> Result<String, RunError> {
            self.after_resume_calls += 1;
            self.after_resume_events.push(event);
            if let Some(message) = &self.after_resume_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.after_resume_output.clone())
        }
    }

    #[derive(Debug, Default)]
    struct FakeSessionBus {
        method_replies: Vec<Result<BusReply, SessionBusError>>,
        process_results: VecDeque<Result<Option<BusSignal>, SessionBusError>>,
        method_calls: Vec<(String, String, String, String)>,
        signal_match_count: usize,
    }

    #[derive(Debug)]
    struct FakeLifecycleBus {
        signals: VecDeque<BusSignal>,
        signal_match_count: usize,
        method_calls: Vec<(String, String, String, String)>,
        inhibitor_write_fds: Vec<i32>,
        disable_config_after_signals: Option<PathBuf>,
        enable_config_after_idle: Option<PathBuf>,
    }

    impl SessionBusClient for FakeSessionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            let _ = name;
            unreachable!("name probing is not used in runner poller tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            self.method_replies.remove(0)
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            let _ = rule;
            self.signal_match_count += 1;
            Ok(())
        }

        fn process(
            &mut self,
            timeout: std::time::Duration,
        ) -> Result<Option<BusSignal>, SessionBusError> {
            let _ = timeout;
            self.process_results
                .pop_front()
                .expect("test should provide every expected bus process result")
        }
    }

    impl SessionBusClient for FakeLifecycleBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            unreachable!("name probing is not used in lifecycle loop tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            let mut pipe_fds = [0; 2];
            let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
            assert_eq!(pipe_result, 0, "test pipe should be created");
            self.inhibitor_write_fds.push(pipe_fds[1]);
            Ok(BusReply::new(vec![BusValue::UnixFd(pipe_fds[0])]))
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.signal_match_count += 1;
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            if let Some(signal) = self.signals.pop_front() {
                return Ok(Some(signal));
            }

            if let Some(config_path) = self.disable_config_after_signals.take() {
                write_lifecycle_config(&config_path, "disabled");
            }
            if let Some(config_path) = self.enable_config_after_idle.take() {
                write_lifecycle_config(&config_path, "enabled");
            }

            Ok(None)
        }
    }

    impl Drop for FakeLifecycleBus {
        fn drop(&mut self) {
            for fd in self.inhibitor_write_fds.drain(..) {
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }

    fn prepare_for_sleep_signal(value: bool) -> BusSignal {
        BusSignal::new(
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "PrepareForSleep",
        )
        .with_sender(LOGIND_SERVICE_NAME)
        .with_body(vec![BusValue::Bool(value)])
    }

    fn logind_owner_changed_signal(old_owner: &str, new_owner: &str) -> BusSignal {
        BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(LOGIND_SERVICE_NAME.to_string()),
                BusValue::String(old_owner.to_string()),
                BusValue::String(new_owner.to_string()),
            ])
    }

    fn locked_hint_changed_signal(session_path: &str, owner: &str, locked: bool) -> BusSignal {
        BusSignal::new(
            session_path,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .with_sender(owner)
        .with_body(vec![
            BusValue::String(LOGIND_SESSION_INTERFACE.to_string()),
            BusValue::Dict(vec![(
                BusValue::String("LockedHint".to_string()),
                BusValue::Variant(Box::new(BusValue::Bool(locked))),
            )]),
            BusValue::Array(Vec::new()),
        ])
    }

    fn graphical_session_properties(uid: u32) -> BusReply {
        let variant = |value| BusValue::Variant(Box::new(value));
        BusReply::new(vec![BusValue::Dict(vec![
            (
                BusValue::String("User".to_string()),
                variant(BusValue::Struct(vec![
                    BusValue::U32(uid),
                    BusValue::ObjectPath(format!("/org/freedesktop/login1/user/_{uid}")),
                ])),
            ),
            (
                BusValue::String("Remote".to_string()),
                variant(BusValue::Bool(false)),
            ),
            (
                BusValue::String("Type".to_string()),
                variant(BusValue::String("wayland".to_string())),
            ),
            (
                BusValue::String("Class".to_string()),
                variant(BusValue::String("user".to_string())),
            ),
            (
                BusValue::String("Active".to_string()),
                variant(BusValue::Bool(true)),
            ),
        ])])
    }

    fn unique_config_path(label: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lg-buddy-{label}-{}-{timestamp}.env",
            std::process::id()
        ))
    }

    fn write_lifecycle_config(path: &Path, policy: &str) {
        fs::write(
            path,
            format!(
                "\
tvs_primary_ip=192.168.1.42
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_1
system_sleep_wake_policy={policy}
"
            ),
        )
        .expect("write lifecycle test config");
    }

    #[test]
    fn idle_event_dispatches_screen_off() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .expect("dispatch idle event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Session became idle."));
        assert!(output.contains("screen-off output"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 0);
        assert_eq!(
            dispatcher.executor.screen_off_events,
            vec![RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::SessionIdle,
            )]
        );
    }

    #[test]
    fn active_and_wake_events_dispatch_screen_on() {
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Active)
            .expect("dispatch active event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::WakeRequested)
            .expect("dispatch wake-requested event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::UserActivity)
            .expect("dispatch user-activity event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("active"));
        assert!(output.contains("wake-requested"));
        assert!(output.contains("user-activity"));
        assert_eq!(dispatcher.executor.screen_off_calls, 0);
        assert_eq!(dispatcher.executor.screen_on_calls, 3);
        assert_eq!(
            dispatcher.executor.screen_on_events,
            vec![
                RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionActive),
                RuntimeEvent::new(
                    EventSource::DesktopSession,
                    RuntimeEventKind::ScreenWakeRequested,
                ),
                RuntimeEvent::new(
                    EventSource::DesktopSession,
                    RuntimeEventKind::UserActivityObserved,
                ),
            ]
        );
    }

    #[test]
    fn lifecycle_events_dispatch_lifecycle_actions() {
        let executor = FakeActionExecutor {
            before_sleep_output: "before-sleep output\n".to_string(),
            after_resume_output: "after-resume output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::BeforeSleep)
            .expect("dispatch before-sleep event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::AfterResume)
            .expect("dispatch after-resume event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("before-sleep"));
        assert!(output.contains("after-resume"));
        assert!(output.contains("before-sleep output"));
        assert!(output.contains("after-resume output"));
        assert_eq!(dispatcher.executor.before_sleep_calls, 1);
        assert_eq!(dispatcher.executor.after_resume_calls, 1);
        assert_eq!(dispatcher.executor.screen_off_calls, 0);
        assert_eq!(dispatcher.executor.screen_on_calls, 0);
    }

    #[test]
    fn lifecycle_monitor_dispatches_logind_sleep_and_reacquires_inhibitor_after_resume() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var(super::LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV, "2");

        let config_path = unique_config_path("lifecycle-monitor");
        write_lifecycle_config(&config_path, "enabled");
        let executor = FakeActionExecutor {
            before_sleep_output: "before-sleep output\n".to_string(),
            after_resume_output: "after-resume output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut bus = FakeLifecycleBus {
            signals: VecDeque::from([
                prepare_for_sleep_signal(true),
                prepare_for_sleep_signal(false),
            ]),
            signal_match_count: 0,
            method_calls: Vec::new(),
            inhibitor_write_fds: Vec::new(),
            disable_config_after_signals: Some(config_path.clone()),
            enable_config_after_idle: None,
        };
        let mut output = Vec::new();

        run_lifecycle_monitor_with_bus(&mut output, executor, &config_path, &mut bus)
            .expect("lifecycle loop exits cleanly after test timeout");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Using logind system lifecycle source"));
        assert!(output.contains("Acquired logind sleep delay inhibitor"));
        assert!(output.contains("System is preparing for sleep"));
        assert!(output.contains("before-sleep output"));
        assert!(output.contains("Released logind sleep delay inhibitor"));
        assert!(output.contains("System resumed from sleep"));
        assert!(output.contains("after-resume output"));
        assert!(!output.contains("diagnostic only"));
        assert!(!output.contains("stopping lifecycle monitor"));
        assert_eq!(bus.signal_match_count, 1);
        assert_eq!(
            bus.method_calls
                .iter()
                .map(|(_, _, _, member)| member.as_str())
                .collect::<Vec<_>>(),
            vec!["Inhibit", "Inhibit"]
        );
        assert!(bus.disable_config_after_signals.is_some());
        fs::remove_file(config_path).expect("remove lifecycle test config");
        std::env::remove_var(super::LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV);
    }

    #[test]
    fn lifecycle_monitor_skips_events_while_policy_is_disabled() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var(super::LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV, "1");

        let config_path = unique_config_path("lifecycle-disabled");
        write_lifecycle_config(&config_path, "disabled");
        let executor = FakeActionExecutor {
            after_resume_output: "after-resume output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut bus = FakeLifecycleBus {
            signals: VecDeque::from([prepare_for_sleep_signal(false)]),
            signal_match_count: 0,
            method_calls: Vec::new(),
            inhibitor_write_fds: Vec::new(),
            disable_config_after_signals: None,
            enable_config_after_idle: None,
        };
        let mut output = Vec::new();

        run_lifecycle_monitor_with_bus(&mut output, executor, &config_path, &mut bus)
            .expect("lifecycle loop exits cleanly after test event limit");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Using logind system lifecycle source"));
        assert!(output.contains("skipping lifecycle event"));
        assert!(!output.contains("System resumed from sleep"));
        assert!(!output.contains("after-resume output"));
        assert_eq!(bus.signal_match_count, 1);
        assert!(bus.method_calls.is_empty());
        fs::remove_file(config_path).expect("remove lifecycle test config");
        std::env::remove_var(super::LIFECYCLE_MONITOR_TEST_EVENT_LIMIT_ENV);
    }

    #[test]
    fn lifecycle_monitor_reacquires_inhibitor_when_policy_is_reenabled_while_idle() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var(super::LIFECYCLE_MONITOR_TEST_TIMEOUT_SECS_ENV, "0.2");

        let config_path = unique_config_path("lifecycle-reenabled");
        write_lifecycle_config(&config_path, "disabled");
        let executor = FakeActionExecutor::default();
        let mut bus = FakeLifecycleBus {
            signals: VecDeque::new(),
            signal_match_count: 0,
            method_calls: Vec::new(),
            inhibitor_write_fds: Vec::new(),
            disable_config_after_signals: None,
            enable_config_after_idle: Some(config_path.clone()),
        };
        let mut output = Vec::new();

        run_lifecycle_monitor_with_bus(&mut output, executor, &config_path, &mut bus)
            .expect("lifecycle loop exits cleanly after test timeout");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Using logind system lifecycle source"));
        assert!(output.contains("Acquired logind sleep delay inhibitor"));
        assert_eq!(bus.signal_match_count, 1);
        assert_eq!(
            bus.method_calls
                .iter()
                .map(|(_, _, _, member)| member.as_str())
                .collect::<Vec<_>>(),
            vec!["Inhibit"]
        );
        assert!(bus.enable_config_after_idle.is_none());
        fs::remove_file(config_path).expect("remove lifecycle test config");
        std::env::remove_var(super::LIFECYCLE_MONITOR_TEST_TIMEOUT_SECS_ENV);
    }

    #[test]
    fn logind_lock_blanks_and_unlock_does_not_restore() {
        let executor = FakeActionExecutor::default();
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event_from_source(&mut output, EventSource::LinuxLogind, SessionEvent::Lock)
            .expect("dispatch lock event");
        dispatcher
            .dispatch_event_from_source(&mut output, EventSource::LinuxLogind, SessionEvent::Unlock)
            .expect("dispatch unlock event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Session locked"));
        assert!(output.contains("no screen restore requested"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 0);
        assert_eq!(
            dispatcher.executor.screen_off_events,
            vec![RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::SessionLocked,
            )]
        );
        assert_eq!(dispatcher.executor.before_sleep_calls, 0);
        assert_eq!(dispatcher.executor.after_resume_calls, 0);
    }

    #[test]
    fn logind_lock_monitor_reconciles_initial_locked_hint() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_session_id = std::env::var_os("XDG_SESSION_ID");
        std::env::set_var("XDG_SESSION_ID", "34");

        let session_path = "/org/freedesktop/login1/session/_34";
        let uid = super::current_effective_uid();
        let mut bus = FakeSessionBus {
            method_replies: vec![
                Ok(BusReply::new(vec![BusValue::ObjectPath(
                    session_path.to_string(),
                )])),
                Ok(graphical_session_properties(uid)),
                Ok(BusReply::new(vec![BusValue::String(":1.42".to_string())])),
                Ok(BusReply::new(vec![BusValue::Variant(Box::new(
                    BusValue::Bool(true),
                ))])),
            ],
            ..FakeSessionBus::default()
        };
        let (sender, receiver) = mpsc::channel();
        let stop = AtomicBool::new(true);

        run_logind_lock_monitor_process(&mut bus, &sender, &stop)
            .expect("initial LockedHint should be reconciled before the signal loop");

        match receiver.recv().expect("initial lock event") {
            RunnerMessage::SessionEvent {
                event,
                source,
                observed_at: _,
            } => {
                assert_eq!(event, SessionEvent::Lock);
                assert_eq!(source, EventSource::LinuxLogind);
            }
            _ => panic!("expected canonical session lock event"),
        }
        assert_eq!(bus.signal_match_count, 2);
        assert_eq!(
            bus.method_calls
                .iter()
                .map(|call| call.3.as_str())
                .collect::<Vec<_>>(),
            vec!["GetSession", "GetAll", "GetNameOwner", "Get"]
        );

        match previous_session_id {
            Some(value) => std::env::set_var("XDG_SESSION_ID", value),
            None => std::env::remove_var("XDG_SESSION_ID"),
        }
    }

    #[test]
    fn logind_lock_monitor_rebinds_after_logind_restarts() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_session_id = std::env::var_os("XDG_SESSION_ID");
        std::env::set_var("XDG_SESSION_ID", "34");

        let session_path = "/org/freedesktop/login1/session/_34";
        let uid = super::current_effective_uid();
        let bind_replies = |owner: &str, locked: bool| {
            vec![
                Ok(BusReply::new(vec![BusValue::ObjectPath(
                    session_path.to_string(),
                )])),
                Ok(graphical_session_properties(uid)),
                Ok(BusReply::new(vec![BusValue::String(owner.to_string())])),
                Ok(BusReply::new(vec![BusValue::Variant(Box::new(
                    BusValue::Bool(locked),
                ))])),
            ]
        };
        let mut bus = FakeSessionBus {
            method_replies: bind_replies(":1.42", false)
                .into_iter()
                .chain(bind_replies(":1.99", false))
                .collect(),
            process_results: VecDeque::from([
                Ok(Some(logind_owner_changed_signal(":1.42", ""))),
                Ok(Some(logind_owner_changed_signal("", ":1.99"))),
                Ok(Some(locked_hint_changed_signal(
                    session_path,
                    ":1.99",
                    true,
                ))),
                Err(SessionBusError::Transport("stop test loop".to_string())),
            ]),
            ..FakeSessionBus::default()
        };
        let (sender, receiver) = mpsc::channel();
        let stop = AtomicBool::new(false);

        assert_eq!(
            run_logind_lock_monitor_process(&mut bus, &sender, &stop),
            Err(LogindSessionError::Bus(SessionBusError::Transport(
                "stop test loop".to_string()
            )))
        );

        match receiver.recv().expect("lock event after logind restart") {
            RunnerMessage::SessionEvent { event, source, .. } => {
                assert_eq!(event, SessionEvent::Lock);
                assert_eq!(source, EventSource::LinuxLogind);
            }
            _ => panic!("expected canonical session lock event"),
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(bus.signal_match_count, 3);
        assert_eq!(
            bus.method_calls
                .iter()
                .map(|call| call.3.as_str())
                .collect::<Vec<_>>(),
            vec![
                "GetSession",
                "GetAll",
                "GetNameOwner",
                "Get",
                "GetSession",
                "GetAll",
                "GetNameOwner",
                "Get",
            ]
        );

        match previous_session_id {
            Some(value) => std::env::set_var("XDG_SESSION_ID", value),
            None => std::env::remove_var("XDG_SESSION_ID"),
        }
    }

    #[test]
    fn unavailable_logind_lock_monitor_reports_diagnostic_without_exiting_native_monitor() {
        let (sender, receiver) = mpsc::channel();

        report_logind_lock_monitor_result(
            &sender,
            Err(LogindSessionError::NoCurrentGraphicalSession),
        );

        match receiver.recv().expect("availability diagnostic") {
            RunnerMessage::Diagnostic(message) => {
                assert!(message.contains("session lock monitoring unavailable"));
                assert!(message.contains("no active local graphical logind session"));
            }
            _ => panic!("optional logind source must not terminate the native monitor"),
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn screen_restore_failures_are_logged_without_stopping_dispatch() {
        let executor = FakeActionExecutor {
            screen_on_error: Some("tv is still waking".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Active)
            .expect("dispatch active event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::WakeRequested)
            .expect("dispatch wake-requested event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen restore action failed. tv is still waking"));
        assert_eq!(dispatcher.executor.screen_on_calls, 2);
    }

    #[test]
    fn screen_off_failures_are_logged_without_stopping_dispatch() {
        let executor = FakeActionExecutor {
            screen_off_error: Some("tv did not respond".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .expect("dispatch idle event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen-off action failed. tv did not respond"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn zero_idle_timeout_falls_back_to_default() {
        assert_eq!(
            normalize_idle_timeout_secs(0),
            crate::config::DEFAULT_IDLE_TIMEOUT
        );
        assert_eq!(normalize_idle_timeout_secs(180), 180);
        assert_eq!(
            normalize_idle_timeout_secs(u128::from(crate::config::MAX_IDLE_TIMEOUT) + 1),
            crate::config::MAX_IDLE_TIMEOUT
        );
    }

    #[test]
    fn invalid_gnome_monitor_timeout_env_values_are_ignored() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        std::env::set_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "0.5");
        assert_eq!(
            super::resolve_gnome_monitor_test_timeout(),
            Some(Duration::from_millis(500))
        );

        std::env::set_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "NaN");
        assert_eq!(super::resolve_gnome_monitor_test_timeout(), None);

        std::env::set_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "inf");
        assert_eq!(super::resolve_gnome_monitor_test_timeout(), None);

        std::env::set_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "0");
        assert_eq!(super::resolve_gnome_monitor_test_timeout(), None);

        std::env::set_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "-1");
        assert_eq!(super::resolve_gnome_monitor_test_timeout(), None);

        std::env::remove_var(super::GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV);
    }

    #[test]
    fn gamepad_activity_source_mode_is_explicitly_selectable() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        std::env::remove_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV);
        std::env::remove_var(super::GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV);
        assert_eq!(
            super::resolve_gamepad_activity_source_mode(),
            super::GamepadActivitySourceMode::System
        );

        std::env::set_var(super::GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV, "0.25");
        assert_eq!(
            super::resolve_gamepad_activity_source_mode(),
            super::GamepadActivitySourceMode::Synthetic(Duration::from_millis(250))
        );

        std::env::set_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV, "disabled");
        assert_eq!(
            super::resolve_gamepad_activity_source_mode(),
            super::GamepadActivitySourceMode::Disabled
        );

        std::env::set_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV, "synthetic");
        assert_eq!(
            super::resolve_gamepad_activity_source_mode(),
            super::GamepadActivitySourceMode::Synthetic(Duration::from_millis(250))
        );

        std::env::set_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV, "system");
        assert_eq!(
            super::resolve_gamepad_activity_source_mode(),
            super::GamepadActivitySourceMode::System
        );

        std::env::remove_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV);
        std::env::remove_var(super::GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV);
    }

    #[test]
    fn latest_inactivity_observation_coalesces_pending_samples() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let first_observed_at = Instant::now();
        let second_observed_at = first_observed_at + Duration::from_millis(250);

        assert!(latest.publish(
            &sender,
            InactivityObservation::DesktopActivityObserved,
            first_observed_at
        ));
        assert!(latest.publish(
            &sender,
            InactivityObservation::DesktopActivityObserved,
            second_observed_at
        ));

        assert!(matches!(
            receiver.recv().expect("notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(TimedInactivityObservation {
                observation: InactivityObservation::DesktopActivityObserved,
                observed_at: second_observed_at,
            })
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn latest_inactivity_observation_notifies_again_after_take() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let first_observed_at = Instant::now();
        let second_observed_at = first_observed_at + Duration::from_millis(250);

        assert!(latest.publish(
            &sender,
            InactivityObservation::DesktopActivityObserved,
            first_observed_at
        ));
        assert!(matches!(
            receiver.recv().expect("first notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(TimedInactivityObservation {
                observation: InactivityObservation::DesktopActivityObserved,
                observed_at: first_observed_at,
            })
        );

        assert!(latest.publish(
            &sender,
            InactivityObservation::DesktopActivityObserved,
            second_observed_at
        ));
        assert!(matches!(
            receiver.recv().expect("second notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(TimedInactivityObservation {
                observation: InactivityObservation::DesktopActivityObserved,
                observed_at: second_observed_at,
            })
        );
    }

    #[test]
    fn gnome_idle_monitor_poller_publishes_recent_desktop_activity() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let mut bus = FakeSessionBus {
            method_replies: vec![Ok(BusReply::new(vec![BusValue::U64(250)]))],
            ..FakeSessionBus::default()
        };
        let before_poll = Instant::now();

        assert!(poll_gnome_idle_monitor_once(&mut bus, &sender, &latest));
        assert!(matches!(
            receiver.recv().expect("notification"),
            RunnerMessage::InactivityObservationReady
        ));
        let observation = latest.take().expect("latest observation");
        assert_eq!(
            observation.observation,
            InactivityObservation::DesktopActivityObserved
        );
        assert!(observation.observed_at < before_poll);
        assert!(observation.observed_at <= Instant::now());
    }

    #[test]
    fn gnome_idle_monitor_poller_does_not_publish_old_activity() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let mut bus = FakeSessionBus {
            method_replies: vec![Ok(BusReply::new(vec![BusValue::U64(1_500)]))],
            ..FakeSessionBus::default()
        };

        assert!(poll_gnome_idle_monitor_once(&mut bus, &sender, &latest));
        assert!(receiver.try_recv().is_err());
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn gnome_idle_monitor_poller_ignores_bus_errors() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let mut bus = FakeSessionBus {
            method_replies: vec![Err(SessionBusError::Transport(
                "simulated bus failure".to_string(),
            ))],
            ..FakeSessionBus::default()
        };

        assert!(poll_gnome_idle_monitor_once(&mut bus, &sender, &latest));
        assert!(receiver.try_recv().is_err());
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn gamepad_activity_send_due_throttles_repeated_activity() {
        let first_sent_at = Instant::now();

        assert!(gamepad_activity_send_due(None, first_sent_at));
        assert!(!gamepad_activity_send_due(
            Some(first_sent_at),
            first_sent_at + GAMEPAD_ACTIVITY_SEND_INTERVAL - Duration::from_millis(1)
        ));
        assert!(gamepad_activity_send_due(
            Some(first_sent_at),
            first_sent_at + GAMEPAD_ACTIVITY_SEND_INTERVAL
        ));
    }

    #[test]
    fn gamepad_refresh_schedule_keeps_earliest_pending_refresh() {
        let now = Instant::now();
        let mut pending_refresh_at = None;

        schedule_gamepad_refresh(&mut pending_refresh_at, now + Duration::from_secs(2));
        schedule_gamepad_refresh(&mut pending_refresh_at, now + Duration::from_secs(5));

        assert_eq!(pending_refresh_at, Some(now + Duration::from_secs(2)));

        schedule_gamepad_refresh(&mut pending_refresh_at, now + Duration::from_secs(1));

        assert_eq!(pending_refresh_at, Some(now + Duration::from_secs(1)));
    }

    #[test]
    fn gamepad_refresh_due_detects_pending_refresh_time() {
        let now = Instant::now();

        assert!(!gamepad_refresh_due(None, now));
        assert!(!gamepad_refresh_due(
            Some(now + Duration::from_millis(1)),
            now
        ));
        assert!(gamepad_refresh_due(Some(now), now));
        assert!(gamepad_refresh_due(
            Some(now),
            now + Duration::from_millis(1)
        ));
    }

    #[test]
    fn completed_gamepad_refresh_can_schedule_short_retry() {
        let now = Instant::now();
        let mut pending_refresh_at = Some(now);

        complete_gamepad_refresh(&mut pending_refresh_at, now, true);

        assert_eq!(
            pending_refresh_at,
            Some(now + GAMEPAD_ACTIVITY_REFRESH_RETRY_INTERVAL)
        );

        complete_gamepad_refresh(&mut pending_refresh_at, now, false);

        assert_eq!(pending_refresh_at, None);
    }

    #[test]
    fn gamepad_device_event_error_emits_diagnostic_and_disables_monitor() {
        let (sender, receiver) = mpsc::channel();
        let mut diagnostics = GamepadDiagnosticEmitter::default();
        let mut monitor = Some(FakeGamepadDeviceEventMonitor {
            result: Some(Err(io::Error::other("boom"))),
        });

        assert_eq!(
            gamepad_device_event_refresh_requested(&sender, &mut diagnostics, &mut monitor),
            GamepadDeviceEventRefresh::NotRequested
        );
        assert!(monitor.is_none());
        match receiver.recv().expect("diagnostic") {
            RunnerMessage::Diagnostic(message) => assert_eq!(
                message,
                "gamepad device event monitor stopped: boom; falling back to periodic reconciliation"
            ),
            _ => panic!("expected diagnostic message"),
        }
    }

    #[test]
    fn gamepad_device_event_error_stops_when_diagnostic_receiver_is_disconnected() {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        let mut diagnostics = GamepadDiagnosticEmitter::default();
        let mut monitor = Some(FakeGamepadDeviceEventMonitor {
            result: Some(Err(io::Error::other("boom"))),
        });

        assert_eq!(
            gamepad_device_event_refresh_requested(&sender, &mut diagnostics, &mut monitor),
            GamepadDeviceEventRefresh::Stop
        );
        assert!(monitor.is_none());
    }

    #[test]
    fn trusted_screen_saver_signals_accept_current_owner_events() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let signal = BusSignal::new(
            GNOME_SCREEN_SAVER_PATH,
            GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::Bool(true)]);

        assert_eq!(trusted.observe(&signal), Some(SessionEvent::Idle));
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_spoofed_senders() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let signal = BusSignal::new(
            GNOME_SCREEN_SAVER_PATH,
            GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        )
        .with_sender(":1.99");

        assert_eq!(trusted.observe(&signal), None);
    }

    #[test]
    fn trusted_screen_saver_signals_update_owner_after_bus_notification() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(true)])
            ),
            None
        );
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.43")
                .with_body(vec![BusValue::Bool(true)])
            ),
            Some(SessionEvent::Idle)
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_untrusted_owner_change_senders() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(":1.99")
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.42")
            ),
            Some(SessionEvent::WakeRequested)
        );
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.43")
            ),
            None
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_events_after_owner_loss() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(String::new()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(false)])
            ),
            None
        );
    }

    #[test]
    fn inactivity_timeout_blanks_once() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(1),
        )
        .expect("blank when the LG Buddy timeout expires");
        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(2),
        )
        .expect("do not blank repeatedly");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Session became idle."));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn desktop_activity_restores_and_resets_the_timeout() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(1),
        )
        .expect("blank when the timeout expires");
        let mut output = Vec::new();

        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::DesktopSession,
            InactivityObservation::DesktopActivityObserved,
            started_at + Duration::from_millis(1_100),
        )
        .expect("restore from desktop activity");
        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(2),
        )
        .expect("activity reset should keep the screen active");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("user-activity"));
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn auxiliary_activity_restores_before_provider_reports_active() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(1),
        )
        .expect("blank when the timeout expires");

        let mut output = Vec::new();
        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::AuxiliaryInput,
            InactivityObservation::UserActivityObserved,
            started_at + Duration::from_millis(1_100),
        )
        .expect("restore from auxiliary activity");
        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::DesktopSession,
            InactivityObservation::ProviderActive,
            started_at + Duration::from_millis(1_200),
        )
        .expect("provider active should not duplicate restore");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("user-activity"));
        assert!(!output.contains("Session event `active` requests screen restore."));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
        assert_eq!(
            dispatcher.executor.screen_on_events,
            vec![RuntimeEvent::new(
                EventSource::AuxiliaryInput,
                RuntimeEventKind::UserActivityObserved,
            )]
        );
    }

    #[test]
    fn native_session_runtime_starts_gamepad_without_a_desktop_provider() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime_dir = unique_config_path("native-session-runtime");
        std::env::set_var("LG_BUDDY_SESSION_RUNTIME_DIR", &runtime_dir);
        std::env::set_var("LG_BUDDY_IDLE_TIMEOUT", "300");
        std::env::set_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV, "synthetic");
        std::env::set_var(super::GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV, "0");

        let (activity_sender, activity_receiver) = mpsc::channel();
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            screen_on_signal: Some(activity_sender),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        let result = run_native_session_monitor_with_lock_monitor(
            &mut output,
            &mut dispatcher,
            ScreenBackend::Auto,
            |sender, _latest_inactivity| {
                thread::spawn(move || {
                    let result = activity_receiver
                        .recv_timeout(Duration::from_secs(1))
                        .map_err(|err| super::SessionRunnerError::Failed {
                            backend: ScreenBackend::Auto,
                            message: format!(
                                "synthetic gamepad activity was not dispatched: {err}"
                            ),
                        });
                    let _ = sender.send(RunnerMessage::MonitorExited(result));
                })
            },
            |_| None,
        );

        std::env::remove_var("LG_BUDDY_SESSION_RUNTIME_DIR");
        std::env::remove_var("LG_BUDDY_IDLE_TIMEOUT");
        std::env::remove_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV);
        std::env::remove_var(super::GAMEPAD_ACTIVITY_TEST_AFTER_SECS_ENV);

        result.expect("shared native session runtime should start and process gamepad activity");

        assert_eq!(dispatcher.executor.screen_on_calls, 1);
        assert_eq!(
            dispatcher.executor.screen_on_events,
            vec![RuntimeEvent::new(
                EventSource::AuxiliaryInput,
                RuntimeEventKind::UserActivityObserved,
            )]
        );
    }

    #[test]
    fn lock_blank_ignores_unlock_signals_and_restores_for_fresh_independent_activity() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime_dir = unique_config_path("native-session-lock-runtime");
        std::env::set_var("LG_BUDDY_SESSION_RUNTIME_DIR", &runtime_dir);
        std::env::set_var("LG_BUDDY_IDLE_TIMEOUT", "300");
        std::env::set_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV, "disabled");

        let executor = FakeActionExecutor::default();
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        let result = run_native_session_monitor_with_lock_monitor(
            &mut output,
            &mut dispatcher,
            ScreenBackend::Auto,
            |sender, _latest_inactivity| {
                thread::spawn(move || {
                    let locked_at = Instant::now();
                    for event in [SessionEvent::Lock, SessionEvent::Lock] {
                        let _ = sender.send(RunnerMessage::SessionEvent {
                            event,
                            source: EventSource::LinuxLogind,
                            observed_at: locked_at,
                        });
                    }
                    let _ = sender.send(RunnerMessage::ActivityObservation {
                        source: EventSource::DesktopSession,
                        observation: InactivityObservation::DesktopActivityObserved,
                        observed_at: locked_at - Duration::from_millis(1),
                    });
                    for event in [SessionEvent::WakeRequested, SessionEvent::Active] {
                        let _ = sender.send(RunnerMessage::SessionEvent {
                            event,
                            source: EventSource::DesktopSession,
                            observed_at: locked_at + Duration::from_millis(1),
                        });
                    }
                    let _ = sender.send(RunnerMessage::SessionEvent {
                        event: SessionEvent::Unlock,
                        source: EventSource::LinuxLogind,
                        observed_at: locked_at + Duration::from_millis(2),
                    });
                    let _ = sender.send(RunnerMessage::SessionEvent {
                        event: SessionEvent::Active,
                        source: EventSource::DesktopSession,
                        observed_at: locked_at + Duration::from_millis(2),
                    });
                    let _ = sender.send(RunnerMessage::ActivityObservation {
                        source: EventSource::DesktopSession,
                        observation: InactivityObservation::DesktopActivityObserved,
                        observed_at: locked_at + Duration::from_millis(3),
                    });
                    let _ = sender.send(RunnerMessage::MonitorExited(Ok(())));
                })
            },
            |_| None,
        );

        std::env::remove_var("LG_BUDDY_SESSION_RUNTIME_DIR");
        std::env::remove_var("LG_BUDDY_IDLE_TIMEOUT");
        std::env::remove_var(super::GAMEPAD_ACTIVITY_SOURCE_ENV);

        result.expect("shared native session runtime should process lock state");
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
        assert_eq!(
            dispatcher.executor.screen_off_events,
            vec![RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::SessionLocked,
            )]
        );
        assert_eq!(
            dispatcher.executor.screen_on_events,
            vec![RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::UserActivityObserved,
            )]
        );
        let output = String::from_utf8(output).expect("utf8");
        assert_eq!(output.matches("Session locked").count(), 1);
        assert!(output.contains("no screen restore requested"));
    }

    #[test]
    fn activity_resets_timeout_before_reblanking() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(1),
        )
        .expect("blank when the timeout expires");

        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::AuxiliaryInput,
            InactivityObservation::UserActivityObserved,
            started_at + Duration::from_millis(1_100),
        )
        .expect("restore from auxiliary activity");

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(2),
        )
        .expect("the original deadline must stay retired");
        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_millis(2_100),
        )
        .expect("blank at the reset deadline");

        assert_eq!(dispatcher.executor.screen_off_calls, 2);
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
    }

    #[test]
    fn failed_blank_is_not_retried_without_activity() {
        let executor = FakeActionExecutor {
            screen_off_error: Some("tv did not respond".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(1),
        )
        .expect("initial blank attempt should be logged");
        handle_inactivity_timeout(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            started_at + Duration::from_secs(2),
        )
        .expect("elapsed time should not retry blank");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen-off action failed. tv did not respond"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn provider_active_restores_once_from_unknown_state() {
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let started_at = Instant::now();
        let mut inactivity = InactivityEngine::new(Duration::from_secs(1), started_at);
        let mut output = Vec::new();

        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::DesktopSession,
            InactivityObservation::ProviderActive,
            started_at,
        )
        .expect("initial provider active should restore");
        handle_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            EventSource::DesktopSession,
            InactivityObservation::ProviderActive,
            started_at + Duration::from_millis(100),
        )
        .expect("repeated provider active should not duplicate restore");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("active"));
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
    }

    #[test]
    fn shell_quote_wraps_path_for_posix_shell() {
        assert_eq!(shell_quote(Path::new("/tmp/lg buddy")), "'/tmp/lg buddy'");
        assert_eq!(
            shell_quote(Path::new("/tmp/that'one")),
            "'/tmp/that'\"'\"'one'"
        );
    }
}
