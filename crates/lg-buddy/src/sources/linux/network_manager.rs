use std::io::Write;

use crate::config::Config;
use crate::events::RuntimeEvent;
use crate::lifecycle::{self, Sleeper};
use crate::session_bus::SessionBusClient;
use crate::state::{ScreenOwnershipMarker, SystemSleepAttemptState};
use crate::tv::TvClient;
use crate::RunError;

use super::logind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkTeardownEvent {
    pub(crate) event: RuntimeEvent,
    pub(crate) phase_read_error: Option<String>,
}

impl NetworkTeardownEvent {
    fn new(machine_sleep_pending: Option<bool>, phase_read_error: Option<String>) -> Self {
        Self {
            event: RuntimeEvent::network_teardown_imminent(machine_sleep_pending),
            phase_read_error,
        }
    }
}

pub(crate) fn network_teardown_event_from_logind_property<B: SessionBusClient>(
    bus: &mut B,
) -> NetworkTeardownEvent {
    match logind::preparing_for_sleep(bus) {
        Ok(preparing) => NetworkTeardownEvent::new(Some(preparing), None),
        Err(err) => NetworkTeardownEvent::new(None, Some(err.to_string())),
    }
}

pub(crate) fn handle_pre_down_with<W: Write, C: TvClient, Sl: Sleeper, B: SessionBusClient>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
    bus: &mut B,
) -> Result<(), RunError> {
    let event = if config.system_sleep_wake_policy.is_enabled() {
        network_teardown_event_from_logind_property(bus)
    } else {
        NetworkTeardownEvent::new(None, None)
    };

    lifecycle::handle_network_teardown_with(
        writer,
        config,
        lifecycle::NetworkTeardownDeps {
            marker,
            attempt_state,
            tv_client,
            sleeper,
        },
        event.event,
        event.phase_read_error.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{handle_pre_down_with, network_teardown_event_from_logind_property};
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenIdleBlankPolicy, ScreenRestorePolicy,
        SystemSleepWakePolicy,
    };
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::lifecycle::Sleeper;
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use crate::state::{
        ScreenOwnershipMarker, SystemSleepAttemptLock, SystemSleepAttemptState,
        SystemSleepCycleOutcome,
    };
    use crate::tv::BscpylgtvCommandClient;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use support::MockBscpylgtv;

    #[derive(Debug)]
    struct FakeBus {
        replies: VecDeque<Result<BusReply, SessionBusError>>,
        calls: Vec<(String, String, String, String, Vec<BusValue>)>,
    }

    impl FakeBus {
        fn preparing_for_sleep(value: bool) -> Self {
            Self {
                replies: VecDeque::from([Ok(BusReply::new(vec![BusValue::Variant(Box::new(
                    BusValue::Bool(value),
                ))]))]),
                calls: Vec::new(),
            }
        }

        fn failing() -> Self {
            Self {
                replies: VecDeque::from([Err(SessionBusError::Transport(
                    "system bus unavailable".to_string(),
                ))]),
                calls: Vec::new(),
            }
        }
    }

    impl SessionBusClient for FakeBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            unreachable!("name probing is not used by NetworkManager gate tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
                call.body,
            ));
            self.replies
                .pop_front()
                .expect("queued PreparingForSleep reply")
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            unreachable!("signal matches are not used by NetworkManager gate tests")
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            unreachable!("signal processing is not used by NetworkManager gate tests")
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

    struct StateWritingSleeper {
        durations: RefCell<Vec<Duration>>,
        attempt_state: SystemSleepAttemptState,
        outcome: SystemSleepCycleOutcome,
        owner: RefCell<Option<SystemSleepAttemptLock>>,
    }

    impl StateWritingSleeper {
        fn new(
            attempt_state: SystemSleepAttemptState,
            outcome: SystemSleepCycleOutcome,
            owner: Option<SystemSleepAttemptLock>,
        ) -> Self {
            Self {
                durations: RefCell::new(Vec::new()),
                attempt_state,
                outcome,
                owner: RefCell::new(owner),
            }
        }

        fn durations(&self) -> Vec<Duration> {
            self.durations.borrow().clone()
        }
    }

    impl Sleeper for StateWritingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
            self.attempt_state
                .write_outcome(self.outcome)
                .expect("write terminal outcome during wait");
            drop(self.owner.borrow_mut().take());
        }
    }

    #[test]
    fn logind_property_read_maps_to_canonical_network_teardown_event() {
        let mut bus = FakeBus::preparing_for_sleep(true);

        let event = network_teardown_event_from_logind_property(&mut bus);

        assert_eq!(
            event.event,
            RuntimeEvent::new(
                EventSource::LinuxNetworkManager,
                RuntimeEventKind::NetworkTeardownImminent {
                    machine_sleep_pending: Some(true),
                },
            )
        );
        assert_eq!(event.phase_read_error, None);
        assert_eq!(bus.calls.len(), 1);
    }

    #[test]
    fn logind_property_read_failure_maps_to_unknown_phase_event() {
        let mut bus = FakeBus::failing();

        let event = network_teardown_event_from_logind_property(&mut bus);

        assert_eq!(event.event, RuntimeEvent::network_teardown_imminent(None));
        assert!(event
            .phase_read_error
            .as_deref()
            .expect("phase read error")
            .contains("system bus unavailable"));
    }

    #[test]
    fn pre_down_returns_immediately_when_logind_is_not_preparing_for_sleep() {
        let temp_dir = TestDir::new("nm-pre-down-not-sleeping");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create stale attempt");
        let mock = MockBscpylgtv::new("nm-pre-down-not-sleeping-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(false);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("ordinary network disconnect should fail open");

        assert!(!marker.exists());
        assert!(!attempt_state.exists());
        assert!(mock.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("not preparing for sleep"));
        assert_eq!(bus.calls.len(), 1);
    }

    #[test]
    fn pre_down_fails_open_when_logind_property_read_fails() {
        let temp_dir = TestDir::new("nm-pre-down-logind-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-logind-failure-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::failing();
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("logind read failure should fail open");

        assert!(!marker.exists());
        assert!(mock.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("failing open"));
    }

    #[test]
    fn pre_down_runs_sleep_power_off_when_logind_is_preparing_for_sleep() {
        let temp_dir = TestDir::new("nm-pre-down-sleeping");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-sleeping-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("sleeping pre-down should power off");

        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(sleeper.durations().is_empty());
        let output = rendered(&output);
        assert!(output.contains("preparing for sleep"));
        assert!(output.contains("Turning off for sleep"));
    }

    #[test]
    fn pre_down_retries_when_network_manager_owned_cycle_reports_retryable_failure() {
        let temp_dir = TestDir::new("nm-pre-down-owned-retryable");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-owned-retryable-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "offline");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("pre-down should retry its own retryable rail failure");

        assert_eq!(
            attempt_state.read_outcome().expect("read cycle outcome"),
            Some(SystemSleepCycleOutcome::Completed)
        );
        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off", "get_input", "power_off"]);
        assert!(rendered(&output).contains("retrying before network teardown"));
    }

    #[test]
    fn pre_down_stops_after_network_manager_owned_follow_up_failure() {
        let temp_dir = TestDir::new("nm-pre-down-owned-follow-up-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-owned-follow-up-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "offline");
        mock.queue_error("power_off", 1, "still offline");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("pre-down should release teardown after its follow-up fails");

        assert_eq!(
            attempt_state.read_outcome().expect("read cycle outcome"),
            Some(SystemSleepCycleOutcome::RetryableTransportFailure)
        );
        assert!(!marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off", "get_input", "power_off"]);
        assert!(rendered(&output).contains("retrying before network teardown"));
    }

    #[test]
    fn pre_down_repeated_sleep_hooks_are_idempotent() {
        let temp_dir = TestDir::new("nm-pre-down-idempotent");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-idempotent-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut first_bus = FakeBus::preparing_for_sleep(true);
        let mut second_bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut first_bus,
        )
        .expect("first sleeping pre-down should power off");
        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut second_bus,
        )
        .expect("repeated sleeping pre-down should remain safe");

        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("preparing for sleep"));
    }

    #[test]
    fn pre_down_waits_when_logind_owned_cycle_is_in_progress() {
        let temp_dir = TestDir::new("nm-pre-down-waits-in-progress");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let _owner = attempt_state
            .try_lock()
            .expect("acquire logind owner lock")
            .expect("logind owner lock should be available");
        attempt_state
            .write_outcome(SystemSleepCycleOutcome::InProgress)
            .expect("write in-progress outcome");
        let mock = MockBscpylgtv::new("nm-pre-down-waits-in-progress-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = StateWritingSleeper::new(
            attempt_state.clone(),
            SystemSleepCycleOutcome::Completed,
            None,
        );
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("pre-down should wait for in-progress rail");

        assert!(!sleeper.durations().is_empty());
        assert_eq!(
            attempt_state.read_outcome().expect("read cycle outcome"),
            Some(SystemSleepCycleOutcome::Completed)
        );
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("suspend rail completed"));
    }

    #[test]
    fn pre_down_releases_teardown_when_in_progress_cycle_times_out() {
        let temp_dir = TestDir::new("nm-pre-down-in-progress-timeout");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let _owner = attempt_state
            .try_lock()
            .expect("acquire logind owner lock")
            .expect("logind owner lock should be available");
        attempt_state
            .write_outcome(SystemSleepCycleOutcome::InProgress)
            .expect("write in-progress outcome");
        let mock = MockBscpylgtv::new("nm-pre-down-in-progress-timeout-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("pre-down should release teardown after bounded wait");

        assert!(!sleeper.durations().is_empty());
        assert_eq!(
            attempt_state.read_outcome().expect("read cycle outcome"),
            Some(SystemSleepCycleOutcome::InProgress)
        );
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("did not finish before deadline"));
    }

    #[test]
    fn pre_down_retries_when_waiting_cycle_reports_retryable_failure() {
        let temp_dir = TestDir::new("nm-pre-down-in-progress-retryable");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let owner = attempt_state
            .try_lock()
            .expect("acquire logind owner lock")
            .expect("logind owner lock should be available");
        attempt_state
            .write_outcome(SystemSleepCycleOutcome::InProgress)
            .expect("write in-progress outcome");
        let mock = MockBscpylgtv::new("nm-pre-down-in-progress-retryable-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = StateWritingSleeper::new(
            attempt_state.clone(),
            SystemSleepCycleOutcome::RetryableTransportFailure,
            Some(owner),
        );
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Enabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("pre-down should retry retryable rail failure");

        assert!(!sleeper.durations().is_empty());
        assert_eq!(
            attempt_state.read_outcome().expect("read cycle outcome"),
            Some(SystemSleepCycleOutcome::Completed)
        );
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("retrying before network teardown"));
    }

    #[test]
    fn pre_down_skips_without_touching_logind_when_policy_is_disabled() {
        let temp_dir = TestDir::new("nm-pre-down-disabled");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("nm-pre-down-disabled-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let mut bus = FakeBus::preparing_for_sleep(true);
        let mut output = Vec::new();

        handle_pre_down_with(
            &mut output,
            &sample_config(SystemSleepWakePolicy::Disabled),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            &mut bus,
        )
        .expect("disabled policy should skip");

        assert!(!marker.exists());
        assert!(mock.calls().is_empty());
        assert!(bus.calls.is_empty());
        assert!(rendered(&output).contains("disabled by config"));
    }

    fn sample_config(system_sleep_wake_policy: SystemSleepWakePolicy) -> Config {
        Config {
            tv_ip: "192.0.2.42".parse::<Ipv4Addr>().expect("parse ipv4"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("parse mac"),
            input: HdmiInput::Hdmi2,
            tv_platform: crate::config::TvPlatform::Bscpylgtv,
            screen_backend: ScreenBackend::Auto,
            screen_idle_blank: ScreenIdleBlankPolicy::Enabled,
            screen_idle_timeout: 300,
            screen_restore_policy: ScreenRestorePolicy::MarkerOnly,
            system_sleep_wake_policy,
        }
    }

    fn client_for_mock(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(
            std::net::Ipv4Addr::new(192, 0, 2, 42),
            mock.command_path(),
            mock.command_args(),
        )
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
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

    struct TestDir {
        path: std::path::PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-{label}-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}
