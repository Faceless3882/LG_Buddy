use std::fmt;
use std::time::{Duration, Instant};

use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;
use crate::session::{SessionEvent, SessionObservation};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, parse_name_owner_changed_signal, BusMethodCall,
    BusSignal, BusSignalMatch, BusValue, SessionBusClient, SessionBusError, DBUS_INTERFACE,
    DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};

const GNOME_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const GNOME_BUS_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const GNOME_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const GNOME_DESKTOP_ACTIVITY_WINDOW: Duration = Duration::from_millis(500);
const GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV: &str = "LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS";

pub const GNOME_SHELL_NAME: &str = "org.gnome.Shell";
pub const GNOME_SCREEN_SAVER_NAME: &str = "org.gnome.ScreenSaver";
pub const GNOME_SCREEN_SAVER_PATH: &str = "/org/gnome/ScreenSaver";
pub const GNOME_SCREEN_SAVER_INTERFACE: &str = "org.gnome.ScreenSaver";
pub const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
pub const GNOME_IDLE_MONITOR_INTERFACE: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_REQUIRED_SERVICES_REASON: &str =
    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct GnomeServiceStatus {
    shell_available: bool,
    screen_saver_available: bool,
    idle_monitor_available: bool,
}

impl GnomeServiceStatus {
    fn can_start(&self) -> bool {
        self.shell_available && self.screen_saver_available && self.idle_monitor_available
    }
}

pub(crate) struct GnomeSource {
    bus: Box<dyn SessionBusClient + Send>,
    trusted_screen_saver_signals: TrustedScreenSaverSignals,
}

#[derive(Debug)]
pub(crate) enum GnomeSourceError {
    Unavailable(&'static str),
    Failed(String),
}

impl fmt::Display for GnomeSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "{reason}"),
            Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for GnomeSourceError {}

impl GnomeSource {
    pub(crate) fn connect() -> Result<Self, GnomeSourceError> {
        let mut bus = new_session_bus_client().map_err(|err| {
            GnomeSourceError::Failed(format!("failed to open GNOME session bus client: {err}"))
        })?;
        bus.wait_for_name(GNOME_SHELL_NAME, GNOME_WAIT_TIMEOUT)
            .map_err(|err| {
                GnomeSourceError::Failed(format!(
                    "failed waiting for GNOME Shell on the session bus: {err}"
                ))
            })?;

        let status = gnome_service_status_from_session_bus(&mut bus);
        if !status.can_start() {
            return Err(GnomeSourceError::Unavailable(
                GNOME_REQUIRED_SERVICES_REASON,
            ));
        }

        subscribe_to_gnome_signals(&mut bus)?;
        let owner = resolve_screen_saver_owner(&mut bus).map_err(|err| {
            GnomeSourceError::Failed(format!("failed to resolve GNOME ScreenSaver owner: {err}"))
        })?;

        Ok(Self {
            bus,
            trusted_screen_saver_signals: TrustedScreenSaverSignals::new(Some(owner)),
        })
    }

    pub(crate) fn run<F>(mut self, mut publish: F) -> Result<(), GnomeSourceError>
    where
        F: FnMut(SessionObservation) -> bool,
    {
        run_gnome_monitor_process(
            &mut self.bus,
            &mut self.trusted_screen_saver_signals,
            &mut publish,
        )
    }
}

pub fn map_screen_saver_signal(signal: &BusSignal) -> Option<SessionEvent> {
    if signal.path != GNOME_SCREEN_SAVER_PATH || signal.interface != GNOME_SCREEN_SAVER_INTERFACE {
        return None;
    }

    match (signal.member.as_str(), signal.body.as_slice()) {
        ("ActiveChanged", [BusValue::Bool(true)]) => Some(SessionEvent::Idle),
        ("ActiveChanged", [BusValue::Bool(false)]) => Some(SessionEvent::Active),
        ("WakeUpScreen", []) => Some(SessionEvent::WakeRequested),
        _ => None,
    }
}

pub fn resolve_screen_saver_owner(
    bus: &mut impl SessionBusClient,
) -> Result<String, SessionBusError> {
    get_name_owner(bus, GNOME_SCREEN_SAVER_NAME)
}

pub fn screen_saver_owner_changed(signal: &BusSignal) -> Option<Option<String>> {
    let owner_change = parse_name_owner_changed_signal(signal)?;
    if owner_change.name != GNOME_SCREEN_SAVER_NAME {
        return None;
    }

    Some(owner_change.new_owner)
}

pub fn current_idle_monitor_idletime_ms(
    bus: &mut impl SessionBusClient,
) -> Result<u64, SessionBusError> {
    bus.call_method(BusMethodCall::new(
        GNOME_IDLE_MONITOR_NAME,
        GNOME_IDLE_MONITOR_PATH,
        GNOME_IDLE_MONITOR_INTERFACE,
        "GetIdletime",
    ))?
    .single_u64()
}

fn gnome_service_status_from_session_bus(bus: &mut impl SessionBusClient) -> GnomeServiceStatus {
    GnomeServiceStatus {
        shell_available: bus.name_has_owner(GNOME_SHELL_NAME).unwrap_or(false),
        screen_saver_available: bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false),
        idle_monitor_available: bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false),
    }
}

fn subscribe_to_gnome_signals(bus: &mut impl SessionBusClient) -> Result<(), GnomeSourceError> {
    bus.add_signal_match(BusSignalMatch {
        sender: None,
        path: Some(GNOME_SCREEN_SAVER_PATH),
        interface: Some(GNOME_SCREEN_SAVER_INTERFACE),
        member: None,
    })
    .map_err(|err| {
        GnomeSourceError::Failed(format!(
            "failed to subscribe to GNOME ScreenSaver signals: {err}"
        ))
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: Some(DBUS_SERVICE_NAME),
        path: Some(DBUS_OBJECT_PATH),
        interface: Some(DBUS_INTERFACE),
        member: Some("NameOwnerChanged"),
    })
    .map_err(|err| {
        GnomeSourceError::Failed(format!("failed to subscribe to D-Bus owner changes: {err}"))
    })
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

fn run_gnome_monitor_process<F>(
    bus: &mut impl SessionBusClient,
    trusted_screen_saver_signals: &mut TrustedScreenSaverSignals,
    publish: &mut F,
) -> Result<(), GnomeSourceError>
where
    F: FnMut(SessionObservation) -> bool,
{
    let started = Instant::now();
    let test_timeout = monitor_test_timeout();
    let mut next_idle_poll = Instant::now();

    loop {
        if test_timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Ok(());
        }

        let now = Instant::now();
        if now >= next_idle_poll {
            if !poll_idle_monitor_once(bus, publish) {
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

        let Some(signal) = bus.process(process_timeout).map_err(|err| {
            GnomeSourceError::Failed(format!("GNOME session bus processing failed: {err}"))
        })?
        else {
            continue;
        };

        let Some(event) = trusted_screen_saver_signals.observe(&signal) else {
            continue;
        };

        if !publish(SessionObservation::Event {
            event,
            source: EventSource::DesktopSession,
            observed_at: Instant::now(),
        }) {
            return Ok(());
        }
    }
}

fn poll_idle_monitor_once<F>(bus: &mut impl SessionBusClient, publish: &mut F) -> bool
where
    F: FnMut(SessionObservation) -> bool,
{
    let Ok(idletime_ms) = current_idle_monitor_idletime_ms(bus) else {
        return true;
    };

    let activity_age = Duration::from_millis(idletime_ms);
    if activity_age > GNOME_DESKTOP_ACTIVITY_WINDOW {
        return true;
    }

    let observed_at = Instant::now();
    let activity_at = observed_at.checked_sub(activity_age).unwrap_or(observed_at);

    publish(SessionObservation::Inactivity {
        observation: InactivityObservation::DesktopActivityObserved,
        source: EventSource::DesktopSession,
        observed_at: activity_at,
    })
}

pub(crate) fn monitor_test_timeout() -> Option<Duration> {
    std::env::var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
}

#[cfg(test)]
mod tests {
    use super::{
        current_idle_monitor_idletime_ms, gnome_service_status_from_session_bus,
        map_screen_saver_signal, monitor_test_timeout, poll_idle_monitor_once,
        resolve_screen_saver_owner, screen_saver_owner_changed, GnomeServiceStatus,
        TrustedScreenSaverSignals, GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV,
    };
    use crate::events::EventSource;
    use crate::session::inactivity::InactivityObservation;
    use crate::session::{SessionEvent, SessionObservation};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    #[derive(Debug, Default)]
    struct FakeSessionBus {
        shell_available: bool,
        screen_saver_available: bool,
        idle_monitor_available: bool,
        idletime_ms: Option<u64>,
        screen_saver_owner: Option<String>,
        method_calls: Vec<(String, String, String, String)>,
        failed_names: Vec<String>,
    }

    impl SessionBusClient for FakeSessionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            if self.failed_names.iter().any(|failed| failed == name) {
                return Err(SessionBusError::Transport(
                    "simulated bus failure".to_string(),
                ));
            }

            match name {
                super::GNOME_SHELL_NAME => Ok(self.shell_available),
                super::GNOME_SCREEN_SAVER_NAME => Ok(self.screen_saver_available),
                super::GNOME_IDLE_MONITOR_NAME => Ok(self.idle_monitor_available),
                _ => Ok(false),
            }
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            match (
                call.destination,
                call.path,
                call.interface,
                call.member,
                self.screen_saver_owner.as_deref(),
                self.idletime_ms,
            ) {
                (
                    DBUS_SERVICE_NAME,
                    DBUS_OBJECT_PATH,
                    DBUS_INTERFACE,
                    "GetNameOwner",
                    Some(value),
                    _,
                ) => Ok(BusReply::new(vec![crate::session_bus::BusValue::String(
                    value.to_string(),
                )])),
                (
                    super::GNOME_IDLE_MONITOR_NAME,
                    super::GNOME_IDLE_MONITOR_PATH,
                    super::GNOME_IDLE_MONITOR_INTERFACE,
                    "GetIdletime",
                    _,
                    Some(value),
                ) => Ok(BusReply::new(vec![crate::session_bus::BusValue::U64(
                    value,
                )])),
                _ => Err(SessionBusError::Transport(
                    "no queued GNOME method reply".to_string(),
                )),
            }
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            let _ = rule;
            unreachable!("not used in GNOME probing tests")
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            let _ = timeout;
            unreachable!("not used in GNOME probing tests")
        }
    }

    #[test]
    fn active_changed_true_signal_maps_to_idle_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(true)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Idle));
    }

    #[test]
    fn active_changed_false_signal_maps_to_active_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(false)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Active));
    }

    #[test]
    fn wakeup_signal_maps_to_wake_requested_event_via_bus_signal() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        );

        assert_eq!(
            map_screen_saver_signal(&signal),
            Some(SessionEvent::WakeRequested)
        );
    }

    #[test]
    fn resolve_screen_saver_owner_uses_generic_name_owner_lookup() {
        let mut bus = FakeSessionBus {
            screen_saver_owner: Some(":1.42".to_string()),
            ..FakeSessionBus::default()
        };

        assert_eq!(
            resolve_screen_saver_owner(&mut bus),
            Ok(":1.42".to_string())
        );
        assert_eq!(
            bus.method_calls,
            vec![(
                DBUS_SERVICE_NAME.to_string(),
                DBUS_OBJECT_PATH.to_string(),
                DBUS_INTERFACE.to_string(),
                "GetNameOwner".to_string(),
            )]
        );
    }

    #[test]
    fn screen_saver_owner_changed_returns_new_owner_for_gnome_service() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.41".to_string()),
                BusValue::String(":1.42".to_string()),
            ]);

        assert_eq!(
            screen_saver_owner_changed(&signal),
            Some(Some(":1.42".to_string()))
        );
    }

    #[test]
    fn screen_saver_owner_changed_ignores_other_services() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.example.Other".to_string()),
                BusValue::String(":1.41".to_string()),
                BusValue::String(":1.42".to_string()),
            ]);

        assert_eq!(screen_saver_owner_changed(&signal), None);
    }

    #[test]
    fn status_from_session_bus_uses_required_gnome_service_names() {
        let mut bus = FakeSessionBus {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: false,
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_service_status_from_session_bus(&mut bus),
            GnomeServiceStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn status_from_session_bus_treats_bus_errors_as_unavailable() {
        let mut bus = FakeSessionBus {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: true,
            failed_names: vec![super::GNOME_IDLE_MONITOR_NAME.to_string()],
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_service_status_from_session_bus(&mut bus),
            GnomeServiceStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn current_idle_monitor_idletime_uses_gnome_idle_monitor_endpoint() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(1_500),
            ..FakeSessionBus::default()
        };

        assert_eq!(current_idle_monitor_idletime_ms(&mut bus), Ok(1_500));
        assert_eq!(
            bus.method_calls,
            vec![(
                super::GNOME_IDLE_MONITOR_NAME.to_string(),
                super::GNOME_IDLE_MONITOR_PATH.to_string(),
                super::GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "GetIdletime".to_string(),
            )]
        );
    }

    #[test]
    fn idle_monitor_poller_publishes_recent_desktop_activity() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(250),
            ..FakeSessionBus::default()
        };
        let before_poll = Instant::now();
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));

        let [SessionObservation::Inactivity {
            observation,
            source,
            observed_at,
        }] = observations.as_slice()
        else {
            panic!("expected one inactivity observation");
        };
        assert_eq!(*observation, InactivityObservation::DesktopActivityObserved);
        assert_eq!(*source, EventSource::DesktopSession);
        assert!(*observed_at < before_poll);
        assert!(*observed_at <= Instant::now());
    }

    #[test]
    fn idle_monitor_poller_does_not_publish_old_activity() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(1_500),
            ..FakeSessionBus::default()
        };
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));
        assert!(observations.is_empty());
    }

    #[test]
    fn idle_monitor_poller_ignores_bus_errors() {
        let mut bus = FakeSessionBus::default();
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));
        assert!(observations.is_empty());
    }

    #[test]
    fn trusted_screen_saver_signals_accept_only_the_current_owner() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let current = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::Bool(true)]);
        let spoofed = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        )
        .with_sender(":1.99");

        assert_eq!(trusted.observe(&current), Some(SessionEvent::Idle));
        assert_eq!(trusted.observe(&spoofed), None);
    }

    #[test]
    fn trusted_screen_saver_signals_follow_owner_changes() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
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
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.43")
                .with_body(vec![BusValue::Bool(true)])
            ),
            Some(SessionEvent::Idle)
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_untrusted_owner_changes_and_owner_loss() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let untrusted_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(":1.99")
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&untrusted_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.42")
            ),
            Some(SessionEvent::WakeRequested)
        );

        let owner_loss = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(String::new()),
            ]);
        assert_eq!(trusted.observe(&owner_loss), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(false)])
            ),
            None
        );
    }

    #[test]
    fn invalid_monitor_timeout_env_values_are_ignored() {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        std::env::set_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "0.5");
        assert_eq!(monitor_test_timeout(), Some(Duration::from_millis(500)));

        for invalid in ["NaN", "inf", "0", "-1"] {
            std::env::set_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, invalid);
            assert_eq!(monitor_test_timeout(), None);
        }

        std::env::remove_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV);
    }
}
