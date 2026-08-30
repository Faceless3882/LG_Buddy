use std::env;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitStatus};
use std::thread;
use std::time::Duration;

use crate::config::{Config, ScreenRestorePolicy};
use crate::events::{RuntimeEvent, RuntimeEventKind};
use crate::policy::{
    ActionKind, DecisionReason, DecisionReasonCode, Diagnostic, PolicyOutcome, StateMarker,
    StateTransition, TransitionReason, TransitionReasonCode,
};
use crate::state::{
    ScreenOwnershipMarker, SystemSleepAttemptLock, SystemSleepAttemptState,
    SystemSleepCycleOutcome, SystemSleepCycleState,
};
use crate::tv::{CurrentInput, TvClient, TvDevice, TvErrorKind};
use crate::wol::{WakeOnLanSender, DEFAULT_WOL_PORT};
use crate::{RunError, StartupMode};

const STARTUP_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
pub(crate) const STARTUP_WAKE_ATTEMPTS: u32 = 6;
const TV_ROUTE_WAIT_ATTEMPTS: u32 = 60;
const TV_ROUTE_WAIT_DELAY: Duration = Duration::from_millis(500);
const SYSTEM_PRE_SLEEP_GET_INPUT_RETRIES: u32 = 0;
const SYSTEM_PRE_SLEEP_POWER_OFF_ATTEMPTS: u32 = 1;
const SYSTEM_SLEEP_GET_INPUT_RETRIES: u32 = 3;
const SYSTEM_SLEEP_POWER_OFF_RETRIES: u32 = 4;
const SYSTEM_SLEEP_RAIL_DEADLINE: Duration = Duration::from_secs(4);
const SYSTEM_SLEEP_RAIL_WAIT_POLL: Duration = Duration::from_millis(50);

pub(crate) trait Sleeper {
    fn sleep(&self, duration: Duration);
}

pub(crate) struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub(crate) trait SleepRequestDetector {
    fn is_sleep_requested(&self) -> io::Result<bool>;
}

pub(crate) trait NetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()>;

    fn wait_for_route_to(&self, _target: Ipv4Addr) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) trait RebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool>;
}

pub(crate) struct JournalctlSleepDetector {
    command_path: PathBuf,
}

pub(crate) struct NmOnlineNetworkWaiter {
    command_path: PathBuf,
}

impl Default for JournalctlSleepDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for NmOnlineNetworkWaiter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl JournalctlSleepDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_JOURNALCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("journalctl")),
        }
    }
}

impl NmOnlineNetworkWaiter {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_NM_ONLINE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("nm-online")),
        }
    }
}

impl SleepRequestDetector for JournalctlSleepDetector {
    fn is_sleep_requested(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-u", "NetworkManager", "-n", "30", "--no-pager"])
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("manager: sleep: sleep requested"))
    }
}

impl NetworkWaiter for NmOnlineNetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-q", "-t", "60"])
            .output()?;

        nm_online_status_result(output.status)
    }

    fn wait_for_route_to(&self, target: Ipv4Addr) -> io::Result<()> {
        wait_for_route_to_target(target)
    }
}

fn nm_online_status_result(status: ExitStatus) -> io::Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("nm-online exited with {status}"),
        ))
    }
}

pub(crate) struct StartupDeps<'a, C, S, Sl, N> {
    pub(crate) tv_client: &'a C,
    pub(crate) wol_sender: &'a S,
    pub(crate) sleeper: &'a Sl,
    pub(crate) network_waiter: &'a N,
}

pub(crate) struct NetworkTeardownDeps<'a, C, Sl> {
    pub(crate) marker: &'a ScreenOwnershipMarker,
    pub(crate) attempt_state: &'a SystemSleepAttemptState,
    pub(crate) tv_client: &'a C,
    pub(crate) sleeper: &'a Sl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LifecyclePolicyDecision<N> {
    next: N,
    outcome: PolicyOutcome,
}

impl<N> LifecyclePolicyDecision<N> {
    fn new(next: N) -> Self {
        Self {
            next,
            outcome: PolicyOutcome::new(),
        }
    }

    fn with_outcome(next: N, outcome: PolicyOutcome) -> Self {
        Self { next, outcome }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupRoute {
    ColdBoot,
    SystemResume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreNext {
    Restore,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownNext {
    QueryInput,
    PowerOff,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RebootObservation {
    Pending,
    NotPending,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvInputObservation {
    Current(CurrentInput),
    QueryFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvEffectObservation {
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepNext {
    PowerOff,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepPowerOffContext {
    KnownInput,
    FallbackAfterInputQueryFailure { marker_was_set: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepAttemptNext {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuspendRailDisposition {
    OwnerCompleted,
    JoinedInProgress,
    DuplicateCompleted,
    RetryableFailure,
}

enum SystemSleepCycleObservation {
    Completed,
    RetryableTransportFailure(SystemSleepAttemptLock),
    InProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkTeardownNext {
    AttemptPreSleep,
    ClearAttempt,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkTeardownPolicyInput {
    policy_enabled: bool,
    machine_sleep_pending: Option<bool>,
    phase_read_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    Startup { mode: StartupMode },
    ShutdownRequested,
    NetworkTeardownImminent { machine_sleep_pending: Option<bool> },
    MachinePreparingForSleep,
    MachineResumed,
}

impl LifecycleEvent {
    pub fn from_runtime_event(event: RuntimeEvent) -> Option<Self> {
        match event.kind {
            RuntimeEventKind::MachineStartup { mode } => Some(Self::Startup { mode }),
            RuntimeEventKind::MachineShutdownRequested => Some(Self::ShutdownRequested),
            RuntimeEventKind::NetworkTeardownImminent {
                machine_sleep_pending,
            } => Some(Self::NetworkTeardownImminent {
                machine_sleep_pending,
            }),
            RuntimeEventKind::MachinePreparingForSleep => Some(Self::MachinePreparingForSleep),
            RuntimeEventKind::MachineResumed => Some(Self::MachineResumed),
            RuntimeEventKind::SessionIdle
            | RuntimeEventKind::SessionActive
            | RuntimeEventKind::SessionLocked
            | RuntimeEventKind::SessionUnlocked
            | RuntimeEventKind::ScreenWakeRequested
            | RuntimeEventKind::UserActivityObserved
            | RuntimeEventKind::ScreenBlankRequested
            | RuntimeEventKind::ScreenRestoreRequested
            | RuntimeEventKind::BrightnessRequested
            | RuntimeEventKind::VolumeRequested => None,
        }
    }
}

fn decide_startup_route(mode: StartupMode, marker_exists: bool) -> StartupRoute {
    if matches!(mode, StartupMode::Wake) || matches!(mode, StartupMode::Auto) && marker_exists {
        StartupRoute::SystemResume
    } else {
        StartupRoute::ColdBoot
    }
}

fn decide_startup_cold_boot() -> PolicyOutcome {
    PolicyOutcome::new()
        .with_state_transition(clear_system_marker(TransitionReasonCode::StartupBoot))
        .with_action(
            ActionKind::TvStartupRestore,
            DecisionReason::new(DecisionReasonCode::ManualRequest),
        )
        .with_action(
            ActionKind::WakeOnLan,
            DecisionReason::new(DecisionReasonCode::ManualRequest),
        )
}

fn select_lifecycle_wake_packet(reason: DecisionReasonCode) -> PolicyOutcome {
    PolicyOutcome::new().with_action(ActionKind::WakeOnLan, DecisionReason::new(reason))
}

fn select_lifecycle_input_restore(reason: DecisionReasonCode) -> PolicyOutcome {
    PolicyOutcome::new().with_action(ActionKind::TvInputRestore, DecisionReason::new(reason))
}

fn decide_restore_after_system_sleep_start(
    policy: ScreenRestorePolicy,
    marker_exists: bool,
) -> LifecyclePolicyDecision<RestoreNext> {
    if !restore_is_allowed(policy, marker_exists) {
        return LifecyclePolicyDecision::with_outcome(
            RestoreNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::MarkerMissing)),
        );
    }

    let mut outcome = PolicyOutcome::new()
        .with_action(
            ActionKind::TvSystemResumeRestore,
            DecisionReason::new(DecisionReasonCode::RuntimeEvent),
        )
        .with_action(
            ActionKind::WakeOnLan,
            DecisionReason::new(DecisionReasonCode::RuntimeEvent),
        );

    if !marker_exists {
        outcome = outcome.with_diagnostic(Diagnostic::info(
            "aggressive markerless system resume restore",
        ));
    }

    LifecyclePolicyDecision::with_outcome(RestoreNext::Restore, outcome)
}

fn decide_restore_input_succeeded() -> PolicyOutcome {
    PolicyOutcome::new()
}

fn decide_restore_input_exhausted(message: &'static str) -> PolicyOutcome {
    PolicyOutcome::new().with_diagnostic(Diagnostic::warning(message))
}

fn decide_system_resume_input_succeeded() -> PolicyOutcome {
    PolicyOutcome::new()
        .with_state_transition(clear_system_marker(TransitionReasonCode::RestoreCompleted))
}

fn decide_system_resume_input_exhausted(message: &'static str) -> PolicyOutcome {
    PolicyOutcome::new()
        .with_diagnostic(Diagnostic::warning(message))
        .with_state_transition(clear_system_marker(TransitionReasonCode::TransportFailure))
}

fn decide_shutdown_after_reboot(
    observation: RebootObservation,
) -> LifecyclePolicyDecision<ShutdownNext> {
    match observation {
        RebootObservation::Pending => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::NotApplicable)),
        ),
        RebootObservation::NotPending => LifecyclePolicyDecision::new(ShutdownNext::QueryInput),
        RebootObservation::Unknown(detail) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::QueryInput,
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "could not determine reboot state: {detail}"
            ))),
        ),
    }
}

fn decide_shutdown_after_input(
    configured_input: crate::config::HdmiInput,
    observation: TvInputObservation,
) -> LifecyclePolicyDecision<ShutdownNext> {
    match observation {
        TvInputObservation::Current(current_input) if current_input.is_hdmi(configured_input) => {
            LifecyclePolicyDecision::with_outcome(
                ShutdownNext::PowerOff,
                PolicyOutcome::new().with_action(
                    ActionKind::TvShutdownPowerOff,
                    DecisionReason::new(DecisionReasonCode::RuntimeEvent),
                ),
            )
        }
        TvInputObservation::Current(current_input) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::Stop,
            PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                DecisionReasonCode::InputMismatch,
                format!(
                    "TV is on {current_input}, not {}",
                    configured_input.as_str()
                ),
            )),
        ),
        TvInputObservation::QueryFailed(_) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::PowerOff,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning("shutdown input query failed"))
                .with_action(
                    ActionKind::TvShutdownPowerOff,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_shutdown_power_off_result(observation: TvEffectObservation) -> PolicyOutcome {
    match observation {
        TvEffectObservation::Succeeded => PolicyOutcome::new(),
        TvEffectObservation::Failed(detail) => PolicyOutcome::new().with_diagnostic(
            Diagnostic::warning(format!("shutdown power_off failed: {detail}")),
        ),
    }
}

fn decide_system_sleep_after_input(
    configured_input: crate::config::HdmiInput,
    observation: TvInputObservation,
) -> LifecyclePolicyDecision<SystemSleepNext> {
    match observation {
        TvInputObservation::Current(current_input) if current_input.is_hdmi(configured_input) => {
            LifecyclePolicyDecision::with_outcome(
                SystemSleepNext::PowerOff,
                PolicyOutcome::new().with_action(
                    ActionKind::TvSystemSleepPowerOff,
                    DecisionReason::new(DecisionReasonCode::RuntimeEvent),
                ),
            )
        }
        TvInputObservation::Current(current_input) => LifecyclePolicyDecision::with_outcome(
            SystemSleepNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::with_detail(
                    DecisionReasonCode::InputMismatch,
                    format!(
                        "TV is on {current_input}, not {}",
                        configured_input.as_str()
                    ),
                ))
                .with_state_transition(clear_system_marker(TransitionReasonCode::InputMismatch)),
        ),
        TvInputObservation::QueryFailed(_) => LifecyclePolicyDecision::with_outcome(
            SystemSleepNext::PowerOff,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning(
                    "input query failed before system sleep",
                ))
                .with_action(
                    ActionKind::TvSystemSleepPowerOff,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_system_sleep_power_off_result(
    observation: TvEffectObservation,
    context: SystemSleepPowerOffContext,
) -> PolicyOutcome {
    match (observation, context) {
        (TvEffectObservation::Succeeded, _) => PolicyOutcome::new()
            .with_state_transition(create_system_marker(TransitionReasonCode::ActionSelected)),
        (TvEffectObservation::Failed(_), SystemSleepPowerOffContext::KnownInput) => {
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(
                "power_off failed on known input; state not set",
            ))
        }
        (
            TvEffectObservation::Failed(_),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: true,
            },
        ) => PolicyOutcome::new()
            .with_state_transition(StateTransition::preserve_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::Other),
            ))
            .with_diagnostic(Diagnostic::warning(
                "fallback power_off failed; preserving existing system marker",
            )),
        (
            TvEffectObservation::Failed(_),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: false,
            },
        ) => PolicyOutcome::new()
            .with_state_transition(clear_system_marker(TransitionReasonCode::TransportFailure))
            .with_diagnostic(Diagnostic::warning(
                "fallback power_off failed after retries; state left unset",
            )),
    }
}

fn decide_system_sleep_attempt_start(
    lock_acquired: bool,
) -> LifecyclePolicyDecision<SystemSleepAttemptNext> {
    if !lock_acquired {
        return LifecyclePolicyDecision::with_outcome(
            SystemSleepAttemptNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(
                    DecisionReasonCode::DuplicateSystemSleepAttempt,
                ))
                .with_state_transition(StateTransition::preserve_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::new(TransitionReasonCode::DuplicateSystemSleepAttempt),
                )),
        );
    }

    LifecyclePolicyDecision::with_outcome(
        SystemSleepAttemptNext::Continue,
        PolicyOutcome::new().with_state_transition(StateTransition::clear_marker(
            StateMarker::SystemSleepAttempt,
            TransitionReason::with_detail(
                TransitionReasonCode::Other,
                "clear stale legacy sleep-attempt marker before pre-sleep handling",
            ),
        )),
    )
}

fn decide_network_teardown(
    input: NetworkTeardownPolicyInput,
) -> LifecyclePolicyDecision<NetworkTeardownNext> {
    if !input.policy_enabled {
        return LifecyclePolicyDecision::with_outcome(
            NetworkTeardownNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::ConfigDisabled)),
        );
    }

    match input.machine_sleep_pending {
        Some(true) => LifecyclePolicyDecision::new(NetworkTeardownNext::AttemptPreSleep),
        Some(false) => LifecyclePolicyDecision::with_outcome(
            NetworkTeardownNext::ClearAttempt,
            PolicyOutcome::new()
                .with_state_transition(clear_system_sleep_attempt_marker(
                    TransitionReasonCode::RuntimePhaseIneligible,
                ))
                .with_no_action(DecisionReason::new(
                    DecisionReasonCode::RuntimePhaseIneligible,
                )),
        ),
        None => {
            let detail = input
                .phase_read_error
                .unwrap_or_else(|| "sleep phase is unknown".to_string());
            LifecyclePolicyDecision::with_outcome(
                NetworkTeardownNext::Stop,
                PolicyOutcome::new()
                    .with_no_action(DecisionReason::with_detail(
                        DecisionReasonCode::RuntimePhaseUnknown,
                        detail.clone(),
                    ))
                    .with_diagnostic(Diagnostic::warning(format!(
                        "could not read logind PreparingForSleep; failing open: {detail}"
                    ))),
            )
        }
    }
}

pub(crate) fn run_startup_with<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: StartupDeps<'_, C, S, Sl, N>,
    mode: StartupMode,
) -> Result<(), RunError> {
    run_startup_with_outcome(writer, config, marker, deps, mode).map(|_| ())
}

pub(crate) fn run_startup_with_outcome<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: StartupDeps<'_, C, S, Sl, N>,
    mode: StartupMode,
) -> Result<PolicyOutcome, RunError> {
    let marker_exists = marker.exists();

    match decide_startup_route(mode, marker_exists) {
        StartupRoute::SystemResume => {
            return restore_after_system_sleep_with_outcome(
                writer,
                config,
                marker,
                deps.tv_client,
                deps.wol_sender,
                deps.sleeper,
                deps.network_waiter,
            );
        }
        StartupRoute::ColdBoot => {}
    }

    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(deps.tv_client, config.tv_ip);
    wait_for_tv_network(writer, deps.network_waiter, config.tv_ip, &mut outcome)?;

    match mode {
        StartupMode::Boot => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
        StartupMode::Auto => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
        StartupMode::Wake => unreachable!("wake mode should be handled by lifecycle restore"),
    }

    let start_outcome = decide_startup_cold_boot();
    apply_system_screen_marker_transitions(marker, &start_outcome)?;
    outcome.merge(start_outcome);
    send_wake_packet(
        writer,
        "LG Buddy Startup",
        &tv,
        deps.wol_sender,
        &config.tv_mac,
    )?;
    deps.sleeper.sleep(startup_initial_wake_delay());

    for attempt in 1..=STARTUP_WAKE_ATTEMPTS {
        outcome.merge(select_lifecycle_input_restore(
            DecisionReasonCode::ManualRequest,
        ));
        match tv.input().set(config.input) {
            Ok(()) => {
                outcome.merge(decide_restore_input_succeeded());
                writeln!(
                    writer,
                    "LG Buddy Startup: TV turned on and set to {}.",
                    config.input.as_str()
                )?;
                writer.flush()?;
                return Ok(outcome);
            }
            Err(err) if err.kind() == TvErrorKind::Authentication => {
                writeln!(
                    writer,
                    "LG Buddy Startup: Stored TV authentication is unavailable; skipping unattended TV control."
                )?;
                writer.flush()?;
                return Ok(outcome);
            }
            Err(_) => {}
        }

        let retry_delay = startup_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Startup: Attempt {attempt} failed, retrying in {}s...",
            retry_delay.as_secs()
        )?;
        outcome.merge(select_lifecycle_wake_packet(
            DecisionReasonCode::ManualRequest,
        ));
        send_wake_packet(
            writer,
            "LG Buddy Startup",
            &tv,
            deps.wol_sender,
            &config.tv_mac,
        )?;
        deps.sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    writer.flush()?;
    outcome.merge(decide_restore_input_exhausted(
        "startup input restore exhausted retries",
    ));
    Ok(outcome)
}

pub(crate) fn run_shutdown_with<W: Write, C: TvClient, R: RebootDetector>(
    writer: &mut W,
    config: &Config,
    tv_client: &C,
    reboot_detector: &R,
) -> Result<(), RunError> {
    run_shutdown_with_outcome(writer, config, tv_client, reboot_detector).map(|_| ())
}

pub(crate) fn run_shutdown_with_outcome<W: Write, C: TvClient, R: RebootDetector>(
    writer: &mut W,
    config: &Config,
    tv_client: &C,
    reboot_detector: &R,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let reboot_observation = match reboot_detector.is_reboot_pending() {
        Ok(true) => RebootObservation::Pending,
        Ok(false) => RebootObservation::NotPending,
        Err(err) => RebootObservation::Unknown(err.to_string()),
    };
    let reboot_decision = decide_shutdown_after_reboot(reboot_observation.clone());
    match reboot_observation {
        RebootObservation::Pending => {
            writeln!(writer, "LG Buddy Shutdown: Reboot; ignoring")?;
            outcome.merge(reboot_decision.outcome);
            return Ok(outcome);
        }
        RebootObservation::NotPending => {}
        RebootObservation::Unknown(err) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not determine reboot state. Continuing shutdown. {err}"
            )?;
        }
    }
    outcome.merge(reboot_decision.outcome);

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match tv.input().current() {
        Ok(current_input) => {
            let decision = decide_shutdown_after_input(
                config.input,
                TvInputObservation::Current(current_input.clone()),
            );
            match decision.next {
                ShutdownNext::PowerOff => {
                    writeln!(
                        writer,
                        "LG Buddy Shutdown: TV is on {}. Turning off for shutdown.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                    let power_off = match tv.power().off() {
                        Ok(_) => TvEffectObservation::Succeeded,
                        Err(err) => TvEffectObservation::Failed(err.to_string()),
                    };
                    log_shutdown_power_off_result(writer, power_off.clone())?;
                    outcome.merge(decide_shutdown_power_off_result(power_off));
                }
                ShutdownNext::Stop => {
                    writeln!(
                        writer,
                        "LG Buddy Shutdown: TV is on {current_input} (not {}). Skipping.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                }
                ShutdownNext::QueryInput => unreachable!("input decision cannot request input"),
            }
        }
        Err(err) if err.kind() == TvErrorKind::Authentication => {
            writeln!(
                writer,
                "LG Buddy Shutdown: Stored TV authentication is unavailable; skipping power_off fallback."
            )?;
            outcome.merge(
                PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                    DecisionReasonCode::NotApplicable,
                    err.to_string(),
                )),
            );
        }
        Err(err) => {
            let decision = decide_shutdown_after_input(
                config.input,
                TvInputObservation::QueryFailed(err.to_string()),
            );
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not query TV input. Proceeding with power_off."
            )?;
            outcome.merge(decision.outcome);
            let power_off = match tv.power().off() {
                Ok(_) => TvEffectObservation::Succeeded,
                Err(err) => TvEffectObservation::Failed(err.to_string()),
            };
            log_shutdown_power_off_result(writer, power_off.clone())?;
            outcome.merge(decide_shutdown_power_off_result(power_off));
        }
    }

    Ok(outcome)
}

#[cfg(test)]
pub(crate) fn attempt_system_sleep_power_off_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<(), RunError> {
    attempt_system_sleep_power_off_with_outcome(writer, config, marker, tv_client, sleeper)
        .map(|_| ())
}

pub(crate) fn attempt_system_sleep_power_off_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(tv_client, config.tv_ip);
    let state_was_set = marker.exists();

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_PRE_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) => {
            let decision = decide_system_sleep_after_input(
                config.input,
                TvInputObservation::Current(current_input.clone()),
            );
            apply_system_screen_marker_transitions(marker, &decision.outcome)?;
            match decision.next {
                SystemSleepNext::PowerOff => {
                    writeln!(
                        writer,
                        "LG Buddy Sleep Pre: TV is on {}. Turning off for sleep.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                    let power_off = match tv.power().off() {
                        Ok(_) => TvEffectObservation::Succeeded,
                        Err(err) => TvEffectObservation::Failed(err.to_string()),
                    };
                    let result_outcome = decide_system_sleep_power_off_result(
                        power_off.clone(),
                        SystemSleepPowerOffContext::KnownInput,
                    );
                    apply_system_screen_marker_transitions(marker, &result_outcome)?;
                    render_system_sleep_power_off_result(
                        writer,
                        power_off,
                        SystemSleepPowerOffContext::KnownInput,
                    )?;
                    outcome.merge(result_outcome);
                }
                SystemSleepNext::Stop => {
                    writeln!(
                        writer,
                        "LG Buddy Sleep Pre: TV is on {current_input} (not {}). Skipping.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                }
            }
        }
        Err(err) if err.kind() == TvErrorKind::Authentication => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Stored TV authentication is unavailable; skipping power_off fallback."
            )?;
            outcome.merge(
                PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                    DecisionReasonCode::NotApplicable,
                    err.to_string(),
                )),
            );
        }
        Err(err) => {
            let decision = decide_system_sleep_after_input(
                config.input,
                TvInputObservation::QueryFailed(err.to_string()),
            );
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Could not query TV input. Attempting power_off fallback."
            )?;
            outcome.merge(decision.outcome);

            let power_off = if retry_power_off(&tv, sleeper, SYSTEM_PRE_SLEEP_POWER_OFF_ATTEMPTS) {
                TvEffectObservation::Succeeded
            } else {
                TvEffectObservation::Failed("power_off failed".to_string())
            };
            let context = SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: state_was_set,
            };
            let result_outcome = decide_system_sleep_power_off_result(power_off.clone(), context);
            apply_system_screen_marker_transitions(marker, &result_outcome)?;
            render_system_sleep_power_off_result(writer, power_off, context)?;
            outcome.merge(result_outcome);
        }
    }

    Ok(outcome)
}

pub(crate) fn handle_system_suspend_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    cycle_state: &SystemSleepCycleState,
    tv_client: &C,
    sleeper: &Sl,
    event: RuntimeEvent,
) -> Result<(PolicyOutcome, SuspendRailDisposition), RunError> {
    match LifecycleEvent::from_runtime_event(event) {
        Some(
            LifecycleEvent::MachinePreparingForSleep
            | LifecycleEvent::NetworkTeardownImminent { .. },
        ) => {}
        Some(_) | None => {
            return Err(RunError::Policy(
                "system suspend rail received a non-suspend lifecycle event".to_string(),
            ));
        }
    }

    let guard = cycle_state.try_lock()?;
    let Some(_guard) = guard else {
        let outcome = duplicate_system_suspend_outcome(format!(
            "another system suspend rail owner is already running within the {:?} rail deadline",
            system_sleep_rail_deadline()
        ));
        let disposition = match cycle_state.read_outcome()? {
            Some(SystemSleepCycleOutcome::Completed) => SuspendRailDisposition::DuplicateCompleted,
            Some(
                SystemSleepCycleOutcome::RetryableTransportFailure
                | SystemSleepCycleOutcome::InProgress,
            )
            | None => SuspendRailDisposition::JoinedInProgress,
        };
        return Ok((outcome, disposition));
    };

    handle_system_suspend_owner_with_outcome(
        writer,
        config,
        marker,
        cycle_state,
        tv_client,
        sleeper,
        _guard,
    )
}

fn handle_system_suspend_owner_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    cycle_state: &SystemSleepCycleState,
    tv_client: &C,
    sleeper: &Sl,
    _guard: SystemSleepAttemptLock,
) -> Result<(PolicyOutcome, SuspendRailDisposition), RunError> {
    if matches!(
        cycle_state.read_outcome()?,
        Some(SystemSleepCycleOutcome::Completed)
    ) {
        return Ok((
            duplicate_system_suspend_outcome("system suspend cycle is already completed"),
            SuspendRailDisposition::DuplicateCompleted,
        ));
    }

    cycle_state.write_outcome(SystemSleepCycleOutcome::InProgress)?;

    let mut outcome = PolicyOutcome::new();
    let start_decision = decide_system_sleep_attempt_start(true);
    apply_system_sleep_attempt_transitions(writer, cycle_state, &start_decision.outcome)?;
    outcome.merge(start_decision.outcome);
    outcome.merge(attempt_system_sleep_power_off_with_outcome(
        writer, config, marker, tv_client, sleeper,
    )?);

    let cycle_outcome = decide_system_sleep_cycle_outcome(&outcome, marker.exists());
    cycle_state.write_outcome(cycle_outcome)?;
    let disposition = match cycle_outcome {
        SystemSleepCycleOutcome::RetryableTransportFailure => {
            SuspendRailDisposition::RetryableFailure
        }
        SystemSleepCycleOutcome::InProgress | SystemSleepCycleOutcome::Completed => {
            SuspendRailDisposition::OwnerCompleted
        }
    };

    Ok((outcome, disposition))
}

pub(crate) fn handle_system_suspend_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    cycle_state: &SystemSleepCycleState,
    tv_client: &C,
    sleeper: &Sl,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    handle_system_suspend_with_outcome(
        writer,
        config,
        marker,
        cycle_state,
        tv_client,
        sleeper,
        event,
    )
    .map(|_| ())
}

#[cfg(test)]
pub(crate) fn attempt_system_sleep_power_off_once_with_outcome<
    W: Write,
    C: TvClient,
    Sl: Sleeper,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let guard = attempt_state.try_lock()?;
    let lock_acquired = guard.is_some();
    let decision = decide_system_sleep_attempt_start(lock_acquired);
    if decision.next == SystemSleepAttemptNext::Stop {
        writeln!(
            writer,
            "LG Buddy Sleep Pre: another system sleep attempt is already running; skipping duplicate pre-sleep handling."
        )?;
        outcome.merge(decision.outcome);
        return Ok(outcome);
    };

    let _guard = guard.expect("lock should be present when acquired");
    apply_system_sleep_attempt_transitions(writer, attempt_state, &decision.outcome)?;
    outcome.merge(decision.outcome);
    outcome.merge(attempt_system_sleep_power_off_with_outcome(
        writer, config, marker, tv_client, sleeper,
    )?);
    Ok(outcome)
}

pub(crate) fn handle_network_teardown_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    deps: NetworkTeardownDeps<'_, C, Sl>,
    event: RuntimeEvent,
    phase_read_error: Option<&str>,
) -> Result<(), RunError> {
    handle_network_teardown_with_outcome(writer, config, deps, event, phase_read_error).map(|_| ())
}

pub(crate) fn handle_network_teardown_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    deps: NetworkTeardownDeps<'_, C, Sl>,
    event: RuntimeEvent,
    phase_read_error: Option<&str>,
) -> Result<PolicyOutcome, RunError> {
    let Some(LifecycleEvent::NetworkTeardownImminent {
        machine_sleep_pending,
    }) = LifecycleEvent::from_runtime_event(event)
    else {
        return Err(RunError::Policy(
            "network teardown handler received a non-network lifecycle event".to_string(),
        ));
    };

    let mut outcome = PolicyOutcome::new();
    let decision = decide_network_teardown(NetworkTeardownPolicyInput {
        policy_enabled: config.system_sleep_wake_policy.is_enabled(),
        machine_sleep_pending,
        phase_read_error: phase_read_error.map(str::to_string),
    });

    match decision.next {
        NetworkTeardownNext::AttemptPreSleep => {
            writeln!(
                writer,
                "LG Buddy NetworkManager: logind is preparing for sleep; running pre-sleep TV handling before network teardown."
            )?;
            outcome.merge(decision.outcome);
            let (rail_outcome, rail_disposition) = handle_system_suspend_with_outcome(
                writer,
                config,
                deps.marker,
                deps.attempt_state,
                deps.tv_client,
                deps.sleeper,
                event,
            )?;
            outcome.merge(rail_outcome);
            handle_network_teardown_rail_disposition(
                writer,
                config,
                deps,
                rail_disposition,
                &mut outcome,
            )?;
        }
        NetworkTeardownNext::ClearAttempt => {
            outcome.merge(decision.outcome);
            if let Err(err) = deps.attempt_state.clear() {
                outcome.diagnostics.push(Diagnostic::warning(format!(
                    "could not clear stale system sleep attempt marker while not sleeping: {err}"
                )));
                writeln!(
                    writer,
                    "LG Buddy NetworkManager: could not clear stale system sleep attempt marker while not sleeping. {err}"
                )?;
            }
            writeln!(
                writer,
                "LG Buddy NetworkManager: logind is not preparing for sleep; leaving network disconnect alone."
            )?;
        }
        NetworkTeardownNext::Stop => {
            render_network_teardown_stop(writer, &decision.outcome)?;
            outcome.merge(decision.outcome);
        }
    }

    Ok(outcome)
}

fn handle_network_teardown_rail_disposition<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    deps: NetworkTeardownDeps<'_, C, Sl>,
    disposition: SuspendRailDisposition,
    outcome: &mut PolicyOutcome,
) -> Result<(), RunError> {
    if disposition != SuspendRailDisposition::JoinedInProgress {
        return Ok(());
    }

    match wait_for_system_sleep_cycle_observation(
        deps.attempt_state,
        deps.sleeper,
        system_sleep_rail_deadline(),
    )? {
        SystemSleepCycleObservation::Completed => {
            writeln!(
                writer,
                "LG Buddy NetworkManager: suspend rail completed; releasing network teardown."
            )?;
        }
        SystemSleepCycleObservation::RetryableTransportFailure(guard) => {
            writeln!(
                writer,
                "LG Buddy NetworkManager: suspend rail reported retryable transport failure; retrying before network teardown."
            )?;
            let (retry_outcome, _) = handle_system_suspend_owner_with_outcome(
                writer,
                config,
                deps.marker,
                deps.attempt_state,
                deps.tv_client,
                deps.sleeper,
                guard,
            )?;
            outcome.merge(retry_outcome);
        }
        SystemSleepCycleObservation::InProgress => {
            writeln!(
                writer,
                "LG Buddy NetworkManager: suspend rail did not finish before deadline; releasing network teardown."
            )?;
            outcome.diagnostics.push(Diagnostic::warning(
                "suspend rail did not finish before NetworkManager teardown deadline",
            ));
        }
    }

    Ok(())
}

fn wait_for_system_sleep_cycle_observation<Sl: Sleeper>(
    cycle_state: &SystemSleepAttemptState,
    sleeper: &Sl,
    deadline: Duration,
) -> Result<SystemSleepCycleObservation, RunError> {
    let mut waited = Duration::ZERO;

    loop {
        match cycle_state.read_outcome()? {
            Some(SystemSleepCycleOutcome::Completed) => {
                return Ok(SystemSleepCycleObservation::Completed);
            }
            Some(SystemSleepCycleOutcome::RetryableTransportFailure) => {
                if let Some(guard) = cycle_state.try_lock()? {
                    return Ok(SystemSleepCycleObservation::RetryableTransportFailure(
                        guard,
                    ));
                }
            }
            Some(SystemSleepCycleOutcome::InProgress) | None => {}
        }

        if waited >= deadline {
            return Ok(SystemSleepCycleObservation::InProgress);
        }

        let remaining = deadline.saturating_sub(waited);
        let sleep_for = SYSTEM_SLEEP_RAIL_WAIT_POLL.min(remaining);
        sleeper.sleep(sleep_for);
        waited += sleep_for;
    }
}

pub(crate) fn attempt_legacy_network_manager_sleep_with<
    W: Write,
    C: TvClient,
    D: SleepRequestDetector,
    Sl: Sleeper,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    detector: &D,
    sleeper: &Sl,
) -> Result<(), RunError> {
    match detector.is_sleep_requested() {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Sleep: Could not determine NetworkManager sleep state. Skipping. {err}"
            )?;
            return Ok(());
        }
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) if !current_input.is_hdmi(config.input) => {
            marker.clear()?;
            return Ok(());
        }
        Err(err) if err.kind() == TvErrorKind::Authentication => {
            writeln!(
                writer,
                "LG Buddy Sleep: Stored TV authentication is unavailable; skipping power_off fallback."
            )?;
            return Ok(());
        }
        Ok(_) | Err(_) => {}
    }

    let state_was_set = marker.exists();

    if retry_power_off(&tv, sleeper, SYSTEM_SLEEP_POWER_OFF_RETRIES) {
        marker.create()?;
    } else if !state_was_set {
        marker.clear()?;
    }

    Ok(())
}

pub(crate) fn restore_after_system_sleep_with<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    network_waiter: &N,
) -> Result<(), RunError> {
    restore_after_system_sleep_with_outcome(
        writer,
        config,
        marker,
        tv_client,
        wol_sender,
        sleeper,
        network_waiter,
    )
    .map(|_| ())
}

pub(crate) fn restore_after_system_sleep_with_outcome<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    network_waiter: &N,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(tv_client, config.tv_ip);
    let marker_exists = marker.exists();

    let start_decision =
        decide_restore_after_system_sleep_start(config.screen_restore_policy, marker_exists);
    render_restore_after_system_sleep_start(writer, marker_exists, &start_decision)?;
    if start_decision.next == RestoreNext::Stop {
        outcome.merge(start_decision.outcome);
        return Ok(outcome);
    }

    wait_for_tv_network(writer, network_waiter, config.tv_ip, &mut outcome)?;
    apply_system_screen_marker_transitions(marker, &start_decision.outcome)?;
    outcome.merge(start_decision.outcome);
    writeln!(writer, "LG Buddy Startup: Sending Wake-on-LAN packet...")?;
    writer.flush()?;
    send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
    writer.flush()?;
    sleeper.sleep(startup_initial_wake_delay());

    for attempt in 1..=STARTUP_WAKE_ATTEMPTS {
        outcome.merge(select_lifecycle_input_restore(
            DecisionReasonCode::RuntimeEvent,
        ));
        match tv.input().set(config.input) {
            Ok(()) => {
                let success_outcome = decide_system_resume_input_succeeded();
                apply_system_screen_marker_transitions(marker, &success_outcome)?;
                outcome.merge(success_outcome);
                writeln!(
                    writer,
                    "LG Buddy Startup: TV turned on and set to {}.",
                    config.input.as_str()
                )?;
                writer.flush()?;
                return Ok(outcome);
            }
            Err(err) if err.kind() == TvErrorKind::Authentication => {
                writeln!(
                    writer,
                    "LG Buddy Startup: Stored TV authentication is unavailable; skipping unattended TV control."
                )?;
                writer.flush()?;
                let unavailable_outcome = decide_system_resume_input_exhausted(
                    "system resume skipped because stored TV authentication is unavailable",
                );
                apply_system_screen_marker_transitions(marker, &unavailable_outcome)?;
                outcome.merge(unavailable_outcome);
                return Ok(outcome);
            }
            Err(_) => {}
        }

        let retry_delay = startup_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Startup: Attempt {attempt} failed, retrying in {}s...",
            retry_delay.as_secs()
        )?;
        writer.flush()?;
        outcome.merge(select_lifecycle_wake_packet(
            DecisionReasonCode::RuntimeEvent,
        ));
        writeln!(writer, "LG Buddy Startup: Sending Wake-on-LAN packet...")?;
        writer.flush()?;
        send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
        writer.flush()?;
        sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    writer.flush()?;
    let exhausted_outcome =
        decide_system_resume_input_exhausted("system resume input restore exhausted retries");
    apply_system_screen_marker_transitions(marker, &exhausted_outcome)?;
    outcome.merge(exhausted_outcome);
    Ok(outcome)
}

fn wait_for_tv_network<W: Write, N: NetworkWaiter>(
    writer: &mut W,
    network_waiter: &N,
    tv_ip: Ipv4Addr,
    outcome: &mut PolicyOutcome,
) -> Result<(), RunError> {
    writeln!(
        writer,
        "LG Buddy Startup: Waiting for NetworkManager connectivity..."
    )?;
    writer.flush()?;

    if let Err(err) = network_waiter.wait_for_network() {
        outcome
            .diagnostics
            .push(Diagnostic::warning(format!("network wait failed: {err}")));
        writeln!(
            writer,
            "LG Buddy Startup: Network wait failed. Continuing anyway. {err}"
        )?;
        writer.flush()?;
        return Ok(());
    }

    writeln!(
        writer,
        "LG Buddy Startup: NetworkManager connectivity is available."
    )?;
    writeln!(
        writer,
        "LG Buddy Startup: Waiting for route to TV at {tv_ip}..."
    )?;
    writer.flush()?;

    if let Err(err) = network_waiter.wait_for_route_to(tv_ip) {
        outcome
            .diagnostics
            .push(Diagnostic::warning(format!("TV route wait failed: {err}")));
        writeln!(
            writer,
            "LG Buddy Startup: TV route wait failed. Continuing anyway. {err}"
        )?;
        writer.flush()?;
        return Ok(());
    }

    writeln!(writer, "LG Buddy Startup: Route to TV is available.")?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn restore_is_allowed(policy: ScreenRestorePolicy, marker_exists: bool) -> bool {
    marker_exists || policy == ScreenRestorePolicy::Aggressive
}

pub(crate) fn log_markerless_restore_notice<W: Write>(
    writer: &mut W,
    prefix: &str,
) -> io::Result<()> {
    writeln!(
        writer,
        "{prefix}: State file not found. Aggressive restore policy is enabled, so LG Buddy will attempt wake anyway."
    )
}

pub(crate) fn send_wake_packet<W: Write, C: TvClient, S: WakeOnLanSender>(
    writer: &mut W,
    prefix: &str,
    tv: &TvDevice<'_, C>,
    wol_sender: &S,
    tv_mac: &crate::config::MacAddress,
) -> Result<(), RunError> {
    if let Err(err) = tv.power().wake(wol_sender, tv_mac) {
        writeln!(
            writer,
            "{prefix}: Wake-on-LAN send failed. Continuing anyway. {err}"
        )?;
    }

    Ok(())
}

fn log_shutdown_power_off_result<W: Write>(
    writer: &mut W,
    observation: TvEffectObservation,
) -> Result<(), RunError> {
    if let TvEffectObservation::Failed(detail) = observation {
        writeln!(
            writer,
            "LG Buddy Shutdown: power_off failed, continuing shutdown. {detail}"
        )?;
    }

    Ok(())
}

fn render_system_sleep_power_off_result<W: Write>(
    writer: &mut W,
    observation: TvEffectObservation,
    context: SystemSleepPowerOffContext,
) -> Result<(), RunError> {
    let TvEffectObservation::Failed(_) = observation else {
        return Ok(());
    };

    match context {
        SystemSleepPowerOffContext::KnownInput => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: power_off failed on known input. State not set."
            )?;
        }
        SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
            marker_was_set: true,
        } => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Fallback power_off failed, but state already set by another hook. Keeping state."
            )?;
        }
        SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
            marker_was_set: false,
        } => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Fallback power_off failed after retries. Leaving state unset."
            )?;
        }
    }

    Ok(())
}

fn render_network_teardown_stop<W: Write>(
    writer: &mut W,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    if outcome
        .no_actions
        .iter()
        .any(|decision| decision.reason.code == DecisionReasonCode::ConfigDisabled)
    {
        writeln!(
            writer,
            "LG Buddy NetworkManager: system sleep/wake handling is disabled by config; skipping pre-down handling."
        )?;
        return Ok(());
    }

    if let Some(decision) = outcome
        .no_actions
        .iter()
        .find(|decision| decision.reason.code == DecisionReasonCode::RuntimePhaseUnknown)
    {
        let detail = decision
            .reason
            .detail
            .as_deref()
            .unwrap_or("sleep phase is unknown");
        writeln!(
            writer,
            "LG Buddy NetworkManager: could not read logind PreparingForSleep; failing open. {detail}"
        )?;
    }

    Ok(())
}

fn render_restore_after_system_sleep_start<W: Write>(
    writer: &mut W,
    marker_exists: bool,
    decision: &LifecyclePolicyDecision<RestoreNext>,
) -> Result<(), RunError> {
    if decision.next == RestoreNext::Stop {
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: TV was not on our input. Skipping."
        )?;
    } else if marker_exists {
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: LG Buddy turned TV off. Restoring."
        )?;
    } else {
        log_markerless_restore_notice(writer, "LG Buddy Startup")?;
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: Restoring display state."
        )?;
    }

    Ok(())
}

fn duplicate_system_suspend_outcome(detail: impl Into<String>) -> PolicyOutcome {
    PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
        DecisionReasonCode::DuplicateSystemSleepAttempt,
        detail,
    ))
}

fn decide_system_sleep_cycle_outcome(
    outcome: &PolicyOutcome,
    marker_exists: bool,
) -> SystemSleepCycleOutcome {
    let power_off_selected = outcome
        .actions
        .iter()
        .any(|action| action.kind == ActionKind::TvSystemSleepPowerOff);

    if power_off_selected && !marker_exists {
        SystemSleepCycleOutcome::RetryableTransportFailure
    } else {
        SystemSleepCycleOutcome::Completed
    }
}

fn apply_system_screen_marker_transitions(
    marker: &ScreenOwnershipMarker,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    for transition in &outcome.state_transitions {
        match transition {
            StateTransition::CreateMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => marker.create()?,
            StateTransition::ClearMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => marker.clear()?,
            StateTransition::PreserveMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => {}
            StateTransition::CreateMarker { .. }
            | StateTransition::ClearMarker { .. }
            | StateTransition::PreserveMarker { .. } => {
                return Err(RunError::Policy(
                    "lifecycle screen marker applier received a non-system-screen transition"
                        .to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn apply_system_sleep_attempt_transitions<W: Write>(
    writer: &mut W,
    attempt_state: &SystemSleepAttemptState,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    for transition in &outcome.state_transitions {
        match transition {
            StateTransition::CreateMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => attempt_state.mark_attempted()?,
            StateTransition::ClearMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => {
                if let Err(err) = attempt_state.clear() {
                    writeln!(
                        writer,
                        "LG Buddy Sleep Pre: could not clear stale system sleep attempt marker before pre-sleep handling. {err}"
                    )?;
                }
            }
            StateTransition::PreserveMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => {}
            StateTransition::CreateMarker { .. }
            | StateTransition::ClearMarker { .. }
            | StateTransition::PreserveMarker { .. } => {
                return Err(RunError::Policy(
                    "lifecycle sleep-attempt applier received a non-attempt transition".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn create_system_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::create_marker(
        StateMarker::SystemScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn clear_system_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::clear_marker(
        StateMarker::SystemScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn clear_system_sleep_attempt_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::clear_marker(
        StateMarker::SystemSleepAttempt,
        TransitionReason::new(reason),
    )
}

fn query_current_input_with_retries<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    retries: u32,
) -> Result<CurrentInput, crate::tv::TvError> {
    let mut last_err = None;

    for attempt in 0..=retries {
        match tv.input().current() {
            Ok(current_input) => return Ok(current_input),
            Err(err) => {
                let authentication_failed = err.kind() == TvErrorKind::Authentication;
                last_err = Some(err);
                if authentication_failed {
                    break;
                }
                if attempt < retries {
                    sleeper.sleep(system_sleep_retry_delay());
                }
            }
        }
    }

    Err(last_err.expect("retry loop should capture a tv error"))
}

fn retry_power_off<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    attempts: u32,
) -> bool {
    for attempt in 1..=attempts {
        match tv.power().off() {
            Ok(()) => return true,
            Err(err) if err.kind() == TvErrorKind::Authentication => return false,
            Err(_) => {}
        }

        if attempt < attempts {
            sleeper.sleep(system_sleep_retry_delay());
        }
    }

    false
}

fn duration_override_secs(env_key: &str, default: Duration) -> Duration {
    env::var(env_key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

pub(crate) fn startup_initial_wake_delay() -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS",
        STARTUP_INITIAL_WAKE_DELAY,
    )
}

pub(crate) fn startup_retry_delay(attempt: u32) -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_RETRY_DELAY_SECS",
        Duration::from_secs(u64::from((attempt * 2).min(30))),
    )
}

pub(crate) fn system_sleep_rail_deadline() -> Duration {
    duration_override_secs(
        "LG_BUDDY_SYSTEM_SLEEP_RAIL_DEADLINE_SECS",
        SYSTEM_SLEEP_RAIL_DEADLINE,
    )
}

fn tv_route_wait_attempts() -> u32 {
    env::var("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|attempts| *attempts > 0)
        .unwrap_or(TV_ROUTE_WAIT_ATTEMPTS)
}

fn tv_route_wait_delay() -> Duration {
    env::var("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(TV_ROUTE_WAIT_DELAY)
}

fn system_sleep_retry_delay() -> Duration {
    duration_override_secs("LG_BUDDY_SLEEP_RETRY_DELAY_SECS", Duration::from_secs(1))
}

fn wait_for_route_to_target(target: Ipv4Addr) -> io::Result<()> {
    wait_for_route_to_target_with(
        target,
        tv_route_wait_attempts(),
        tv_route_wait_delay(),
        check_route_to_target,
        thread::sleep,
    )
}

fn wait_for_route_to_target_with<P, S>(
    target: Ipv4Addr,
    attempts: u32,
    retry_delay: Duration,
    mut probe: P,
    mut sleep: S,
) -> io::Result<()>
where
    P: FnMut(Ipv4Addr) -> io::Result<()>,
    S: FnMut(Duration),
{
    let mut last_error = None;

    for attempt in 1..=attempts {
        match probe(target) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                if attempt < attempts {
                    sleep(retry_delay);
                }
            }
        }
    }

    let last_error = last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no route check was attempted for {target}"),
        )
    });
    Err(io::Error::new(
        last_error.kind(),
        format!("route to {target} unavailable after {attempts} attempt(s): {last_error}"),
    ))
}

fn check_route_to_target(target: Ipv4Addr) -> io::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(SocketAddrV4::new(target, DEFAULT_WOL_PORT))
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        attempt_system_sleep_power_off_once_with_outcome,
        attempt_system_sleep_power_off_with_outcome, decide_network_teardown,
        decide_restore_after_system_sleep_start, decide_shutdown_after_input,
        decide_shutdown_after_reboot, decide_startup_route, decide_system_sleep_after_input,
        decide_system_sleep_attempt_start, decide_system_sleep_cycle_outcome,
        decide_system_sleep_power_off_result, handle_network_teardown_with_outcome,
        handle_system_suspend_with_outcome, nm_online_status_result,
        restore_after_system_sleep_with_outcome, run_shutdown_with_outcome,
        run_startup_with_outcome, wait_for_route_to_target_with, LifecycleEvent,
        NetworkTeardownDeps, NetworkTeardownNext, NetworkTeardownPolicyInput, NetworkWaiter,
        RebootDetector, RebootObservation, RestoreNext, ShutdownNext, Sleeper, StartupDeps,
        StartupRoute, SuspendRailDisposition, SystemSleepAttemptNext, SystemSleepNext,
        SystemSleepPowerOffContext, TvEffectObservation, TvInputObservation,
    };
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenIdleBlankPolicy, ScreenRestorePolicy,
        SystemSleepWakePolicy,
    };
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::policy::{
        ActionKind, DecisionReasonCode, StateMarker, StateTransition, TransitionReason,
        TransitionReasonCode,
    };
    use crate::state::{
        ScreenOwnershipMarker, SystemSleepAttemptLock, SystemSleepAttemptState,
        SystemSleepCycleOutcome, SystemSleepCycleState,
    };
    use crate::tv::{BscpylgtvCommandClient, CurrentInput};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use crate::StartupMode;
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process::{self, ExitStatus};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};
    use support::{MockBscpylgtv, TestEnv};

    #[test]
    fn pure_startup_policy_routes_from_mode_and_marker_observation() {
        assert_eq!(
            decide_startup_route(StartupMode::Boot, true),
            StartupRoute::ColdBoot
        );
        assert_eq!(
            decide_startup_route(StartupMode::Wake, false),
            StartupRoute::SystemResume
        );
        assert_eq!(
            decide_startup_route(StartupMode::Auto, true),
            StartupRoute::SystemResume
        );
        assert_eq!(
            decide_startup_route(StartupMode::Auto, false),
            StartupRoute::ColdBoot
        );
    }

    #[test]
    fn pure_network_teardown_policy_selects_sleep_gate_behavior() {
        let pending = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: Some(true),
            phase_read_error: None,
        });

        assert_eq!(pending.next, NetworkTeardownNext::AttemptPreSleep);
        assert!(pending.outcome.actions.is_empty());
        assert!(pending.outcome.no_actions.is_empty());

        let ordinary_disconnect = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: Some(false),
            phase_read_error: None,
        });

        assert_eq!(ordinary_disconnect.next, NetworkTeardownNext::ClearAttempt);
        assert_eq!(
            ordinary_disconnect.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert_eq!(
            ordinary_disconnect.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::RuntimePhaseIneligible),
            )]
        );

        let disabled = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: false,
            machine_sleep_pending: Some(true),
            phase_read_error: None,
        });

        assert_eq!(disabled.next, NetworkTeardownNext::Stop);
        assert_eq!(
            disabled.outcome.no_actions[0].reason.code,
            DecisionReasonCode::ConfigDisabled
        );

        let unknown_phase = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: None,
            phase_read_error: Some("dbus unavailable".to_string()),
        });

        assert_eq!(unknown_phase.next, NetworkTeardownNext::Stop);
        assert_eq!(
            unknown_phase.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseUnknown
        );
        assert_eq!(unknown_phase.outcome.diagnostics.len(), 1);
    }

    #[test]
    fn pure_system_sleep_policy_selects_actions_and_marker_transitions_from_observations() {
        let matching_input = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi3)),
        );

        assert_eq!(matching_input.next, SystemSleepNext::PowerOff);
        assert_eq!(
            matching_input.outcome.actions[0].kind,
            ActionKind::TvSystemSleepPowerOff
        );

        let different_input = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi1)),
        );

        assert_eq!(different_input.next, SystemSleepNext::Stop);
        assert_eq!(
            different_input.outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );
        assert_eq!(
            different_input.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::InputMismatch),
            )]
        );

        let power_off_success = decide_system_sleep_power_off_result(
            TvEffectObservation::Succeeded,
            SystemSleepPowerOffContext::KnownInput,
        );

        assert_eq!(
            power_off_success.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );

        let query_failure = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::QueryFailed("timeout".to_string()),
        );

        assert_eq!(query_failure.next, SystemSleepNext::PowerOff);
        assert_eq!(
            query_failure.outcome.actions[0].reason.code,
            DecisionReasonCode::TransportFailure
        );
        assert_eq!(query_failure.outcome.diagnostics.len(), 1);

        let fallback_failure_preserves_marker = decide_system_sleep_power_off_result(
            TvEffectObservation::Failed("timeout".to_string()),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: true,
            },
        );

        assert_eq!(
            fallback_failure_preserves_marker.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::Other),
            )]
        );
    }

    #[test]
    fn pure_system_sleep_attempt_policy_only_dedupes_concurrent_handlers() {
        let blocked_by_lock = decide_system_sleep_attempt_start(false);

        assert_eq!(blocked_by_lock.next, SystemSleepAttemptNext::Stop);
        assert_eq!(
            blocked_by_lock.outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );
        assert_eq!(
            blocked_by_lock.outcome.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::DuplicateSystemSleepAttempt),
            )]
        );

        let first_attempt = decide_system_sleep_attempt_start(true);

        assert_eq!(first_attempt.next, SystemSleepAttemptNext::Continue);
        assert_eq!(
            first_attempt.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::with_detail(
                    TransitionReasonCode::Other,
                    "clear stale legacy sleep-attempt marker before pre-sleep handling",
                ),
            )]
        );
    }

    #[test]
    fn pure_restore_policy_uses_marker_state_and_restore_mode_as_inputs() {
        let conservative_missing =
            decide_restore_after_system_sleep_start(ScreenRestorePolicy::MarkerOnly, false);

        assert_eq!(conservative_missing.next, RestoreNext::Stop);
        assert_eq!(
            conservative_missing.outcome.no_actions[0].reason.code,
            DecisionReasonCode::MarkerMissing
        );

        let aggressive_missing =
            decide_restore_after_system_sleep_start(ScreenRestorePolicy::Aggressive, false);

        assert_eq!(aggressive_missing.next, RestoreNext::Restore);
        assert_eq!(
            aggressive_missing.outcome.actions[0].kind,
            ActionKind::TvSystemResumeRestore
        );
        assert!(aggressive_missing.outcome.state_transitions.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn nm_online_status_result_reports_nonzero_status() {
        use std::os::unix::process::ExitStatusExt;

        let err = nm_online_status_result(ExitStatus::from_raw(1 << 8))
            .expect_err("nonzero nm-online status should fail the wait");

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("nm-online exited with"));
    }

    #[test]
    fn route_wait_retries_until_tv_route_becomes_available() {
        let target = ip("192.0.2.42");
        let retry_delay = Duration::from_millis(500);
        let mut probes = Vec::new();
        let mut sleeps = Vec::new();

        wait_for_route_to_target_with(
            target,
            3,
            retry_delay,
            |candidate| {
                probes.push(candidate);
                if probes.len() < 3 {
                    Err(io::Error::new(
                        io::ErrorKind::NetworkUnreachable,
                        "route not ready",
                    ))
                } else {
                    Ok(())
                }
            },
            |duration| sleeps.push(duration),
        )
        .expect("route should become available on the third probe");

        assert_eq!(probes, vec![target, target, target]);
        assert_eq!(sleeps, vec![retry_delay, retry_delay]);
    }

    #[test]
    fn pure_shutdown_policy_uses_reboot_and_input_observations() {
        let reboot = decide_shutdown_after_reboot(RebootObservation::Pending);

        assert_eq!(reboot.next, ShutdownNext::Stop);
        assert_eq!(
            reboot.outcome.no_actions[0].reason.code,
            DecisionReasonCode::NotApplicable
        );

        let matching_input = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi1)),
        );

        assert_eq!(matching_input.next, ShutdownNext::PowerOff);
        assert_eq!(
            matching_input.outcome.actions[0].kind,
            ActionKind::TvShutdownPowerOff
        );

        let unknown_reboot_state =
            decide_shutdown_after_reboot(RebootObservation::Unknown("journal unavailable".into()));

        assert_eq!(unknown_reboot_state.next, ShutdownNext::QueryInput);
        assert_eq!(unknown_reboot_state.outcome.diagnostics.len(), 1);

        let mismatched_input = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi4)),
        );

        assert_eq!(mismatched_input.next, ShutdownNext::Stop);
        assert_eq!(
            mismatched_input.outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );

        let query_failure = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::QueryFailed("timeout".to_string()),
        );

        assert_eq!(query_failure.next, ShutdownNext::PowerOff);
        assert_eq!(
            query_failure.outcome.actions[0].reason.code,
            DecisionReasonCode::TransportFailure
        );
        assert_eq!(query_failure.outcome.diagnostics.len(), 1);
    }

    #[test]
    fn lifecycle_events_map_from_canonical_runtime_events() {
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineStartup {
                    mode: StartupMode::Boot,
                },
            )),
            Some(LifecycleEvent::Startup {
                mode: StartupMode::Boot,
            })
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::network_teardown_imminent(Some(true))),
            Some(LifecycleEvent::NetworkTeardownImminent {
                machine_sleep_pending: Some(true),
            })
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::from_logind_prepare_for_sleep(true)),
            Some(LifecycleEvent::MachinePreparingForSleep)
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::from_logind_prepare_for_sleep(false)),
            Some(LifecycleEvent::MachineResumed)
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::SessionIdle,
            )),
            None
        );
    }

    #[test]
    fn system_sleep_power_off_outcome_records_action_and_system_marker_creation() {
        let temp_dir = TestDir::new("lifecycle-sleep-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-sleep-outcome-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = attempt_system_sleep_power_off_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &sleeper,
        )
        .expect("system sleep power off should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn stale_system_sleep_attempt_marker_does_not_block_pre_sleep_handling() {
        let temp_dir = TestDir::new("lifecycle-stale-sleep-attempt");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create stale attempt marker");
        let mock = MockBscpylgtv::new("lifecycle-stale-sleep-attempt-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = attempt_system_sleep_power_off_once_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
        )
        .expect("stale system sleep attempt marker should not block pre-sleep handling");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![
                StateTransition::clear_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::with_detail(
                        TransitionReasonCode::Other,
                        "clear stale legacy sleep-attempt marker before pre-sleep handling",
                    ),
                ),
                StateTransition::create_marker(
                    StateMarker::SystemScreenOwnership,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
            ]
        );
        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn system_suspend_rail_owner_completes_cycle_and_duplicate_skips_tv_work() {
        let temp_dir = TestDir::new("lifecycle-suspend-rail-completed");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let cycle_state = SystemSleepCycleState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-suspend-rail-completed-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();
        let event =
            RuntimeEvent::from_command(crate::Command::SleepPre).expect("sleep-pre runtime event");

        let mut first_output = Vec::new();
        let (first_outcome, first_disposition) = handle_system_suspend_with_outcome(
            &mut first_output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &cycle_state,
            &client,
            &sleeper,
            event,
        )
        .expect("first rail owner should complete");

        assert_eq!(first_disposition, SuspendRailDisposition::OwnerCompleted);
        assert_eq!(
            cycle_state.read_outcome().expect("read completed outcome"),
            Some(SystemSleepCycleOutcome::Completed)
        );
        assert!(marker.exists());
        assert_eq!(
            first_outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(!cycle_state.exists());

        let mut second_output = Vec::new();
        let (second_outcome, second_disposition) = handle_system_suspend_with_outcome(
            &mut second_output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &cycle_state,
            &client,
            &sleeper,
            event,
        )
        .expect("duplicate rail caller should skip");

        assert_eq!(
            second_disposition,
            SuspendRailDisposition::DuplicateCompleted
        );
        assert!(second_outcome.actions.is_empty());
        assert_eq!(
            second_outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn system_suspend_rail_duplicate_observes_in_progress_owner() {
        let temp_dir = TestDir::new("lifecycle-suspend-rail-in-progress");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let cycle_state = SystemSleepCycleState::new(temp_dir.path().to_path_buf());
        let _owner = cycle_state
            .try_lock()
            .expect("acquire owner lock")
            .expect("owner lock should be available");
        cycle_state
            .write_outcome(SystemSleepCycleOutcome::InProgress)
            .expect("write in-progress outcome");
        let mock = MockBscpylgtv::new("lifecycle-suspend-rail-in-progress-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let (outcome, disposition) = handle_system_suspend_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &cycle_state,
            &client,
            &sleeper,
            RuntimeEvent::from_logind_prepare_for_sleep(true),
        )
        .expect("duplicate rail caller should observe in-progress owner");

        assert_eq!(disposition, SuspendRailDisposition::JoinedInProgress);
        assert!(outcome.actions.is_empty());
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );
        assert_call_commands(&mock, &[]);
    }

    #[test]
    fn system_suspend_rail_records_retryable_transport_failure() {
        let temp_dir = TestDir::new("lifecycle-suspend-rail-retryable");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let cycle_state = SystemSleepCycleState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-suspend-rail-retryable-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "offline");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let (outcome, disposition) = handle_system_suspend_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &cycle_state,
            &client,
            &sleeper,
            RuntimeEvent::from_logind_prepare_for_sleep(true),
        )
        .expect("rail owner should record retryable failure");

        assert_eq!(disposition, SuspendRailDisposition::RetryableFailure);
        assert_eq!(
            cycle_state.read_outcome().expect("read retryable outcome"),
            Some(SystemSleepCycleOutcome::RetryableTransportFailure)
        );
        assert!(!marker.exists());
        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn system_sleep_cycle_outcome_treats_noop_as_completed_and_failed_action_as_retryable() {
        let noop = crate::policy::PolicyOutcome::new().with_no_action(
            crate::policy::DecisionReason::new(DecisionReasonCode::InputMismatch),
        );
        assert_eq!(
            decide_system_sleep_cycle_outcome(&noop, false),
            SystemSleepCycleOutcome::Completed
        );

        let failed_action = crate::policy::PolicyOutcome::new().with_action(
            ActionKind::TvSystemSleepPowerOff,
            crate::policy::DecisionReason::new(DecisionReasonCode::RuntimeEvent),
        );
        assert_eq!(
            decide_system_sleep_cycle_outcome(&failed_action, false),
            SystemSleepCycleOutcome::RetryableTransportFailure
        );
        assert_eq!(
            decide_system_sleep_cycle_outcome(&failed_action, true),
            SystemSleepCycleOutcome::Completed
        );
    }

    #[test]
    fn system_sleep_rail_deadline_has_default_and_env_override() {
        let mut env = TestEnv::new();
        env.remove("LG_BUDDY_SYSTEM_SLEEP_RAIL_DEADLINE_SECS");
        assert_eq!(super::system_sleep_rail_deadline(), Duration::from_secs(4));

        env.set("LG_BUDDY_SYSTEM_SLEEP_RAIL_DEADLINE_SECS", "9");
        assert_eq!(super::system_sleep_rail_deadline(), Duration::from_secs(9));
    }

    #[test]
    fn network_teardown_pending_sleep_records_cleanup_and_sleep_power_off() {
        let temp_dir = TestDir::new("lifecycle-nm-pending-sleep");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-pending-sleep-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            NetworkTeardownDeps {
                marker: &marker,
                attempt_state: &attempt_state,
                tv_client: &client,
                sleeper: &sleeper,
            },
            RuntimeEvent::network_teardown_imminent(Some(true)),
            None,
        )
        .expect("pending sleep teardown should power off once");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![
                StateTransition::clear_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::with_detail(
                        TransitionReasonCode::Other,
                        "clear stale legacy sleep-attempt marker before pre-sleep handling",
                    ),
                ),
                StateTransition::create_marker(
                    StateMarker::SystemScreenOwnership,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
            ]
        );
        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("logind is preparing for sleep"));
    }

    #[test]
    fn network_teardown_waits_for_retryable_owner_before_retrying() {
        let temp_dir = TestDir::new("lifecycle-nm-retryable-owner");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let owner = attempt_state
            .try_lock()
            .expect("acquire owner lock")
            .expect("owner lock should be available");
        attempt_state
            .write_outcome(SystemSleepCycleOutcome::RetryableTransportFailure)
            .expect("write retryable outcome");
        let mock = MockBscpylgtv::new("lifecycle-nm-retryable-owner-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = ReleasingSleeper::new(owner);

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            NetworkTeardownDeps {
                marker: &marker,
                attempt_state: &attempt_state,
                tv_client: &client,
                sleeper: &sleeper,
            },
            RuntimeEvent::network_teardown_imminent(Some(true)),
            None,
        )
        .expect("pending sleep teardown should wait and retry");

        assert!(!sleeper.durations().is_empty());
        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            attempt_state
                .read_outcome()
                .expect("read completed outcome"),
            Some(SystemSleepCycleOutcome::Completed)
        );
        assert!(marker.exists());
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("retrying before network teardown"));
    }

    #[test]
    fn network_teardown_ordinary_disconnect_records_no_action_and_clears_attempt() {
        let temp_dir = TestDir::new("lifecycle-nm-not-sleeping");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create stale attempt marker");
        let mock = MockBscpylgtv::new("lifecycle-nm-not-sleeping-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            NetworkTeardownDeps {
                marker: &marker,
                attempt_state: &attempt_state,
                tv_client: &client,
                sleeper: &sleeper,
            },
            RuntimeEvent::network_teardown_imminent(Some(false)),
            None,
        )
        .expect("ordinary network disconnect should fail open");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::RuntimePhaseIneligible),
            )]
        );
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("not preparing for sleep"));
    }

    #[test]
    fn network_teardown_unknown_phase_records_fail_open_no_action() {
        let temp_dir = TestDir::new("lifecycle-nm-unknown-phase");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-unknown-phase-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            NetworkTeardownDeps {
                marker: &marker,
                attempt_state: &attempt_state,
                tv_client: &client,
                sleeper: &sleeper,
            },
            RuntimeEvent::network_teardown_imminent(None),
            Some("system bus unavailable"),
        )
        .expect("unknown sleep phase should fail open");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseUnknown
        );
        assert_eq!(outcome.diagnostics.len(), 1);
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("failing open"));
    }

    #[test]
    fn network_teardown_disabled_policy_records_no_action_without_state_change() {
        let temp_dir = TestDir::new("lifecycle-nm-disabled");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-disabled-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut config = sample_config(HdmiInput::Hdmi2);
        config.system_sleep_wake_policy = SystemSleepWakePolicy::Disabled;
        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &config,
            NetworkTeardownDeps {
                marker: &marker,
                attempt_state: &attempt_state,
                tv_client: &client,
                sleeper: &sleeper,
            },
            RuntimeEvent::network_teardown_imminent(Some(true)),
            None,
        )
        .expect("disabled policy should skip");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::ConfigDisabled
        );
        assert!(outcome.state_transitions.is_empty());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("disabled by config"));
    }

    #[test]
    fn system_resume_outcome_records_restore_actions_and_system_marker_clear() {
        let temp_dir = TestDir::new("lifecycle-resume-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create system marker");
        let mock = MockBscpylgtv::new("lifecycle-resume-outcome-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();

        let mut output = Vec::new();
        let outcome = restore_after_system_sleep_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
            &network,
        )
        .expect("system resume restore should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                ActionKind::TvSystemResumeRestore,
                ActionKind::WakeOnLan,
                ActionKind::TvInputRestore,
            ]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::RestoreCompleted),
            )]
        );
        assert!(!marker.exists());
        assert_eq!(wol.calls().len(), 1);
        assert_eq!(sleeper.durations(), vec![Duration::from_secs(6)]);
        assert_eq!(network.calls(), 1);
        assert_eq!(network.route_targets(), vec![ip("192.0.2.42")]);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
    }

    #[test]
    fn system_resume_logs_route_wait_failure_and_still_attempts_restore() {
        let temp_dir = TestDir::new("lifecycle-resume-route-wait-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create system marker");
        let mock = MockBscpylgtv::new("lifecycle-resume-route-wait-failure-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network =
            FakeNetworkWaiter::failing_route(io::ErrorKind::NetworkUnreachable, "no route");

        let mut output = Vec::new();
        restore_after_system_sleep_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
            &network,
        )
        .expect("route wait failure should not block restore retries");

        assert_eq!(network.calls(), 1);
        assert_eq!(network.route_targets(), vec![ip("192.0.2.42")]);
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Waiting for NetworkManager connectivity"));
        assert!(rendered.contains("Waiting for route to TV at 192.0.2.42"));
        assert!(rendered.contains("TV route wait failed. Continuing anyway."));
    }

    #[test]
    fn startup_boot_outcome_records_restore_actions_and_system_marker_clear() {
        let temp_dir = TestDir::new("lifecycle-startup-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale system marker");
        let mock = MockBscpylgtv::new("lifecycle-startup-outcome-tv");
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
        let outcome = run_startup_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup boot should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                ActionKind::TvStartupRestore,
                ActionKind::WakeOnLan,
                ActionKind::TvInputRestore,
            ]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::StartupBoot),
            )]
        );
        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(network.route_targets(), vec![ip("192.0.2.42")]);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
    }

    #[test]
    fn startup_boot_waits_for_tv_route_before_wake_on_lan() {
        let temp_dir = TestDir::new("lifecycle-startup-route-before-wol");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-startup-route-before-wol-tv");
        let client = client_for_mock(&mock);
        let events = RefCell::new(Vec::new());
        let wol = OrderedWakeOnLanSender { events: &events };
        let sleeper = RecordingSleeper::default();
        let network = OrderedNetworkWaiter { events: &events };

        let mut output = Vec::new();
        run_startup_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            StartupDeps {
                tv_client: &client,
                wol_sender: &wol,
                sleeper: &sleeper,
                network_waiter: &network,
            },
            StartupMode::Boot,
        )
        .expect("startup boot should succeed");

        assert_eq!(*events.borrow(), vec!["network", "route", "wol"]);
    }

    #[test]
    fn startup_boot_logs_route_wait_failure_and_still_attempts_restore() {
        let temp_dir = TestDir::new("lifecycle-startup-route-wait-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-startup-route-wait-failure-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network =
            FakeNetworkWaiter::failing_route(io::ErrorKind::NetworkUnreachable, "no route");

        let mut output = Vec::new();
        run_startup_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            StartupDeps {
                tv_client: &client,
                wol_sender: &wol,
                sleeper: &sleeper,
                network_waiter: &network,
            },
            StartupMode::Boot,
        )
        .expect("route wait failure should not block startup restore");

        assert_eq!(network.calls(), 1);
        assert_eq!(network.route_targets(), vec![ip("192.0.2.42")]);
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input", "get_power_state"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Waiting for route to TV at 192.0.2.42"));
        assert!(rendered.contains("TV route wait failed. Continuing anyway."));
    }

    #[test]
    fn shutdown_outcome_records_power_off_action_for_configured_input() {
        let mock = MockBscpylgtv::new("lifecycle-shutdown-outcome-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let reboot = FakeRebootDetector::clear();

        let mut output = Vec::new();
        let outcome = run_shutdown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &client,
            &reboot,
        )
        .expect("shutdown should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvShutdownPowerOff]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(outcome.state_transitions.is_empty());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    fn sample_config(input: HdmiInput) -> Config {
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
            screen_restore_policy: ScreenRestorePolicy::MarkerOnly,
            system_sleep_wake_policy: SystemSleepWakePolicy::Enabled,
        }
    }

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("parse ipv4")
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

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
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

    struct OrderedWakeOnLanSender<'a> {
        events: &'a RefCell<Vec<&'static str>>,
    }

    impl WakeOnLanSender for OrderedWakeOnLanSender<'_> {
        fn send_magic_packet(&self, _mac: &MacAddress) -> Result<(), WakeOnLanError> {
            self.events.borrow_mut().push("wol");
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

    struct ReleasingSleeper {
        durations: RefCell<Vec<Duration>>,
        owner: RefCell<Option<SystemSleepAttemptLock>>,
    }

    impl ReleasingSleeper {
        fn new(owner: SystemSleepAttemptLock) -> Self {
            Self {
                durations: RefCell::new(Vec::new()),
                owner: RefCell::new(Some(owner)),
            }
        }

        fn durations(&self) -> Vec<Duration> {
            self.durations.borrow().clone()
        }
    }

    impl Sleeper for ReleasingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
            drop(self.owner.borrow_mut().take());
        }
    }

    struct FakeNetworkWaiter {
        network_calls: RefCell<u32>,
        route_targets: RefCell<Vec<Ipv4Addr>>,
        network_result: io::Result<()>,
        route_result: io::Result<()>,
    }

    impl FakeNetworkWaiter {
        fn clear() -> Self {
            Self {
                network_calls: RefCell::new(0),
                route_targets: RefCell::new(Vec::new()),
                network_result: Ok(()),
                route_result: Ok(()),
            }
        }

        fn failing_route(kind: io::ErrorKind, message: &str) -> Self {
            Self {
                network_calls: RefCell::new(0),
                route_targets: RefCell::new(Vec::new()),
                network_result: Ok(()),
                route_result: Err(io::Error::new(kind, message.to_string())),
            }
        }

        fn calls(&self) -> u32 {
            *self.network_calls.borrow()
        }

        fn route_targets(&self) -> Vec<Ipv4Addr> {
            self.route_targets.borrow().clone()
        }
    }

    impl NetworkWaiter for FakeNetworkWaiter {
        fn wait_for_network(&self) -> io::Result<()> {
            *self.network_calls.borrow_mut() += 1;
            match &self.network_result {
                Ok(()) => Ok(()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }

        fn wait_for_route_to(&self, target: Ipv4Addr) -> io::Result<()> {
            self.route_targets.borrow_mut().push(target);
            match &self.route_result {
                Ok(()) => Ok(()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct OrderedNetworkWaiter<'a> {
        events: &'a RefCell<Vec<&'static str>>,
    }

    impl NetworkWaiter for OrderedNetworkWaiter<'_> {
        fn wait_for_network(&self) -> io::Result<()> {
            self.events.borrow_mut().push("network");
            Ok(())
        }

        fn wait_for_route_to(&self, _target: Ipv4Addr) -> io::Result<()> {
            self.events.borrow_mut().push("route");
            Ok(())
        }
    }

    struct FakeRebootDetector {
        pending: io::Result<bool>,
    }

    impl FakeRebootDetector {
        fn clear() -> Self {
            Self { pending: Ok(false) }
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
