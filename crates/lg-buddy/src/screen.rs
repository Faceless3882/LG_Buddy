use std::env;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{
    load_config, resolve_config_path_from_env, Config, HdmiInput, MacAddress, ScreenRestorePolicy,
    TvPlatform,
};
use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
use crate::policy::{
    ActionKind, DecisionReason, DecisionReasonCode, Diagnostic, PolicyOutcome, StateMarker,
    StateTransition, TransitionReason, TransitionReasonCode,
};
#[cfg(test)]
use crate::runtime_phase::NoopRuntimePhaseProvider;
use crate::runtime_phase::{LogindRuntimePhaseProvider, RuntimePhaseProvider, RuntimePhaseRead};
use crate::state::{ScreenOwnershipMarker, StateScope};
use crate::tv::{
    build_tv_client, CurrentInput, TvClient, TvClientBuildOptions, TvDevice, TvError, TvErrorKind,
    TvPowerState,
};
use crate::wol::{UdpWakeOnLanSender, WakeOnLanSender};
use crate::RunError;

const SCREEN_ON_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
const SCREEN_ON_WAKE_ATTEMPTS: u32 = 6;
const MACHINE_SLEEP_PENDING_DETAIL: &str = "machine sleep is pending";
const SYSTEM_RESTORE_PENDING_DETAIL: &str = "system resume restore is pending";

pub(crate) trait Sleeper {
    fn sleep(&self, duration: Duration);
}

pub(crate) struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub(crate) trait SystemLifecycleStatusProvider {
    fn system_restore_pending(&self) -> bool;
}

pub(crate) struct SystemMarkerLifecycleStatusProvider {
    marker: ScreenOwnershipMarker,
}

impl SystemMarkerLifecycleStatusProvider {
    fn from_env() -> Result<Self, crate::state::StateDirError> {
        Ok(Self {
            marker: ScreenOwnershipMarker::from_env(StateScope::System)?,
        })
    }
}

impl SystemLifecycleStatusProvider for SystemMarkerLifecycleStatusProvider {
    fn system_restore_pending(&self) -> bool {
        self.marker.exists()
    }
}

#[cfg(test)]
pub(crate) struct NoopSystemLifecycleStatusProvider;

#[cfg(test)]
impl SystemLifecycleStatusProvider for NoopSystemLifecycleStatusProvider {
    fn system_restore_pending(&self) -> bool {
        false
    }
}

pub(crate) struct ScreenOnDeps<'a, C, S, Sl, P, L> {
    pub(crate) tv_client: &'a C,
    pub(crate) wol_sender: &'a S,
    pub(crate) sleeper: &'a Sl,
    pub(crate) phase_provider: &'a mut P,
    pub(crate) lifecycle_status: &'a L,
}

struct ScreenOnWakeDeps<'a, C, S, Sl> {
    marker: &'a ScreenOwnershipMarker,
    marker_exists: bool,
    tv: &'a TvDevice<'a, C>,
    wol_sender: &'a S,
    sleeper: &'a Sl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenPolicyDecision<N> {
    next: N,
    outcome: PolicyOutcome,
}

impl<N> ScreenPolicyDecision<N> {
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
enum ScreenEligibilityNext {
    Continue,
    Stop(ScreenActionBlockReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenActionBlockReason {
    MachineSleepPending,
    SystemRestorePending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenActionEligibilityInput {
    source: EventSource,
    lifecycle_policy_enabled: bool,
    runtime_phase: RuntimePhaseRead,
    system_restore_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenOffInputObservation {
    Current(CurrentInput),
    QueryFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenOffNext {
    Blank,
    PowerOffFallback,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvEffectObservation {
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenOffFallbackContext {
    InputQueryFailed,
    BlankFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenOnNext {
    Unblank,
    FullWake,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenOnUnblankObservation {
    VerifiedActive,
    SubstateMismatch(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TvFailureSnapshot {
    kind: TvErrorKind,
    detail: String,
}

impl TvFailureSnapshot {
    fn from_error(error: &TvError) -> Self {
        Self {
            kind: error.kind(),
            detail: compact_failure_detail(error.detail()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvStepOutcome {
    Succeeded,
    Failed(TvFailureSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimedTvStep {
    elapsed: Duration,
    outcome: TvStepOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimedPowerStateObservation {
    stage: String,
    elapsed: Duration,
    result: Result<TvPowerState, TvFailureSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WakePacketSnapshot {
    elapsed: Duration,
    error: Option<String>,
}

#[derive(Debug)]
struct ScreenRestoreAnomaly {
    source: EventSource,
    platform: TvPlatform,
    configured_input: HdmiInput,
    marker_before: bool,
    started_at: Instant,
    direct_unblank: Option<TimedTvStep>,
    wake_packets: Vec<WakePacketSnapshot>,
    input_attempts: Vec<TimedTvStep>,
    recovery_unblank_attempts: Vec<TimedTvStep>,
    power_state_observations: Vec<TimedPowerStateObservation>,
}

impl ScreenRestoreAnomaly {
    fn new(event: RuntimeEvent, config: &Config, marker_before: bool) -> Self {
        Self {
            source: event.source,
            platform: config.tv_platform,
            configured_input: config.input,
            marker_before,
            started_at: Instant::now(),
            direct_unblank: None,
            wake_packets: Vec::new(),
            input_attempts: Vec::new(),
            recovery_unblank_attempts: Vec::new(),
            power_state_observations: Vec::new(),
        }
    }

    fn command_failed(&self) -> bool {
        self.direct_unblank
            .as_ref()
            .is_some_and(|step| matches!(&step.outcome, TvStepOutcome::Failed(_)))
            || self
                .input_attempts
                .iter()
                .chain(&self.recovery_unblank_attempts)
                .any(|step| matches!(&step.outcome, TvStepOutcome::Failed(_)))
    }

    fn render<W: Write>(
        &self,
        writer: &mut W,
        reconciliation_result: &Result<(), RunError>,
        marker_after: bool,
    ) -> io::Result<()> {
        writeln!(writer, "LG Buddy Screen Restore Anomaly:")?;
        writeln!(
            writer,
            "  context: source={} platform={} configured_input={} marker_before={}",
            event_source_label(self.source),
            self.platform,
            self.configured_input.as_str(),
            marker_state(self.marker_before),
        )?;
        writeln!(
            writer,
            "  direct_unblank: {}",
            self.direct_unblank
                .as_ref()
                .map(render_tv_step)
                .unwrap_or_else(|| "not_attempted".to_string())
        )?;
        writeln!(
            writer,
            "  wake_packets: {}",
            render_wake_packets(&self.wake_packets)
        )?;
        writeln!(
            writer,
            "  input_attempts: {}",
            render_tv_steps(&self.input_attempts)
        )?;
        writeln!(
            writer,
            "  recovery_unblank_attempts: {}",
            render_tv_steps(&self.recovery_unblank_attempts)
        )?;
        writeln!(
            writer,
            "  power_state_observations: {}",
            render_power_state_observations(&self.power_state_observations)
        )?;
        let reconciliation_result = match reconciliation_result {
            Ok(()) => "verified_active".to_string(),
            Err(error) => format!(
                "failed detail={:?}",
                compact_failure_detail(&error.to_string())
            ),
        };
        writeln!(
            writer,
            "  internal_outcome: reconciliation={} marker_after={} total_elapsed_ms={}",
            reconciliation_result,
            marker_state(marker_after),
            self.started_at.elapsed().as_millis(),
        )
    }
}

pub(crate) fn run_screen_off_from_env<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_screen_off_from_env_for_event(
        writer,
        RuntimeEvent::new(EventSource::CliApi, RuntimeEventKind::ScreenBlankRequested),
    )
}

pub(crate) fn run_screen_off_from_env_for_event<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production(),
    )?;
    let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
    let lifecycle_status =
        SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

    run_screen_off_with_event(
        writer,
        &config,
        &marker,
        &tv_client,
        event,
        &mut phase_provider,
        &lifecycle_status,
    )
}

pub(crate) fn run_screen_on_from_env<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_screen_on_from_env_for_event(
        writer,
        RuntimeEvent::new(
            EventSource::CliApi,
            RuntimeEventKind::ScreenRestoreRequested,
        ),
    )
}

pub(crate) fn run_screen_on_from_env_for_event<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production(),
    )?;
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
    let lifecycle_status =
        SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

    run_screen_on_with_event(
        writer,
        &config,
        &marker,
        ScreenOnDeps {
            tv_client: &tv_client,
            wol_sender: &wol_sender,
            sleeper: &sleeper,
            phase_provider: &mut phase_provider,
            lifecycle_status: &lifecycle_status,
        },
        event,
    )
}

#[cfg(test)]
pub(crate) fn run_screen_off_with<W: Write>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &impl TvClient,
) -> Result<(), RunError> {
    let mut phase_provider = NoopRuntimePhaseProvider;
    let lifecycle_status = NoopSystemLifecycleStatusProvider;
    run_screen_off_with_event(
        writer,
        config,
        marker,
        tv_client,
        RuntimeEvent::new(EventSource::CliApi, RuntimeEventKind::ScreenBlankRequested),
        &mut phase_provider,
        &lifecycle_status,
    )
}

pub(crate) fn run_screen_off_with_event<
    W: Write,
    C: TvClient,
    P: RuntimePhaseProvider,
    L: SystemLifecycleStatusProvider,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    event: RuntimeEvent,
    phase_provider: &mut P,
    lifecycle_status: &L,
) -> Result<(), RunError> {
    run_screen_off_with_outcome_for_event(
        writer,
        config,
        marker,
        tv_client,
        event,
        phase_provider,
        lifecycle_status,
    )
    .map(|_| ())
}

#[cfg(test)]
pub(crate) fn run_screen_off_with_outcome<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
) -> Result<PolicyOutcome, RunError> {
    let mut phase_provider = NoopRuntimePhaseProvider;
    let lifecycle_status = NoopSystemLifecycleStatusProvider;
    run_screen_off_with_outcome_for_event(
        writer,
        config,
        marker,
        tv_client,
        RuntimeEvent::new(EventSource::CliApi, RuntimeEventKind::ScreenBlankRequested),
        &mut phase_provider,
        &lifecycle_status,
    )
}

pub(crate) fn run_screen_off_with_outcome_for_event<
    W: Write,
    C: TvClient,
    P: RuntimePhaseProvider,
    L: SystemLifecycleStatusProvider,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    event: RuntimeEvent,
    phase_provider: &mut P,
    lifecycle_status: &L,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    if !apply_screen_action_eligibility(
        writer,
        "LG Buddy Screen Off",
        config,
        event,
        phase_provider,
        lifecycle_status,
        &mut outcome,
    )? {
        return Ok(outcome);
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match tv.input().current() {
        Ok(current_input) => {
            let decision = decide_screen_off_after_input(
                config.input,
                ScreenOffInputObservation::Current(current_input.clone()),
            );
            apply_screen_state_transitions(marker, &decision.outcome)?;
            render_screen_off_input_decision(writer, config.input, &current_input, &decision)?;
            let next = decision.next;
            outcome.merge(decision.outcome);
            if next == ScreenOffNext::Blank {
                execute_screen_blank(writer, config, marker, &tv, &mut outcome)?;
            }
        }
        Err(err) => {
            let detail = err.to_string();
            let decision = decide_screen_off_after_input(
                config.input,
                ScreenOffInputObservation::QueryFailed(detail.clone()),
            );
            writeln!(
                writer,
                "LG Buddy Screen Off: Could not query TV input. Falling back to power_off. {err}"
            )?;
            let next = decision.next;
            outcome.merge(decision.outcome);
            if next == ScreenOffNext::PowerOffFallback {
                execute_screen_off_power_off_fallback(
                    writer,
                    marker,
                    &tv,
                    ScreenOffFallbackContext::InputQueryFailed,
                    &mut outcome,
                )?;
            }
        }
    }

    Ok(outcome)
}

#[cfg(test)]
pub(crate) fn run_screen_on_with<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
) -> Result<(), RunError> {
    let mut phase_provider = NoopRuntimePhaseProvider;
    let lifecycle_status = NoopSystemLifecycleStatusProvider;
    run_screen_on_with_event(
        writer,
        config,
        marker,
        ScreenOnDeps {
            tv_client,
            wol_sender,
            sleeper,
            phase_provider: &mut phase_provider,
            lifecycle_status: &lifecycle_status,
        },
        RuntimeEvent::new(
            EventSource::CliApi,
            RuntimeEventKind::ScreenRestoreRequested,
        ),
    )
}

pub(crate) fn run_screen_on_with_event<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    P: RuntimePhaseProvider,
    L: SystemLifecycleStatusProvider,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: ScreenOnDeps<'_, C, S, Sl, P, L>,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    run_screen_on_with_outcome_for_event(writer, config, marker, deps, event).map(|_| ())
}

#[cfg(test)]
pub(crate) fn run_screen_on_with_outcome<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
) -> Result<PolicyOutcome, RunError> {
    let mut phase_provider = NoopRuntimePhaseProvider;
    let lifecycle_status = NoopSystemLifecycleStatusProvider;
    run_screen_on_with_outcome_for_event(
        writer,
        config,
        marker,
        ScreenOnDeps {
            tv_client,
            wol_sender,
            sleeper,
            phase_provider: &mut phase_provider,
            lifecycle_status: &lifecycle_status,
        },
        RuntimeEvent::new(
            EventSource::CliApi,
            RuntimeEventKind::ScreenRestoreRequested,
        ),
    )
}

pub(crate) fn run_screen_on_with_outcome_for_event<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    P: RuntimePhaseProvider,
    L: SystemLifecycleStatusProvider,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: ScreenOnDeps<'_, C, S, Sl, P, L>,
    event: RuntimeEvent,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    if !apply_screen_action_eligibility(
        writer,
        "LG Buddy Screen On",
        config,
        event,
        deps.phase_provider,
        deps.lifecycle_status,
        &mut outcome,
    )? {
        return Ok(outcome);
    }

    let marker_exists = marker.exists();
    let mut anomaly = ScreenRestoreAnomaly::new(event, config, marker_exists);
    let start_decision = decide_screen_on_start(config.screen_restore_policy, marker_exists);
    render_screen_on_start_decision(writer, config, marker_exists, &start_decision)?;
    let next = start_decision.next;
    outcome.merge(start_decision.outcome);
    if next == ScreenOnNext::Stop {
        return Ok(outcome);
    }

    let tv = TvDevice::new(deps.tv_client, config.tv_ip);
    if next == ScreenOnNext::Unblank
        && execute_screen_on_unblank(writer, marker, &tv, &mut outcome, &mut anomaly)?
    {
        if anomaly.command_failed() {
            anomaly.render(writer, &Ok(()), marker.exists())?;
        }
        return Ok(outcome);
    }

    let fallback_result = execute_screen_on_full_wake(
        writer,
        config,
        ScreenOnWakeDeps {
            marker,
            marker_exists,
            tv: &tv,
            wol_sender: deps.wol_sender,
            sleeper: deps.sleeper,
        },
        &mut outcome,
        &mut anomaly,
    );
    anomaly.render(writer, &fallback_result, marker.exists())?;
    fallback_result?;
    Ok(outcome)
}

fn decide_screen_action_eligibility(
    input: ScreenActionEligibilityInput,
) -> ScreenPolicyDecision<ScreenEligibilityNext> {
    if !is_session_screen_source(input.source) || !input.lifecycle_policy_enabled {
        return ScreenPolicyDecision::new(ScreenEligibilityNext::Continue);
    }

    match input.runtime_phase {
        RuntimePhaseRead::Pending => ScreenPolicyDecision::with_outcome(
            ScreenEligibilityNext::Stop(ScreenActionBlockReason::MachineSleepPending),
            PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                DecisionReasonCode::RuntimePhaseIneligible,
                MACHINE_SLEEP_PENDING_DETAIL,
            )),
        ),
        RuntimePhaseRead::NotPending => {
            if input.system_restore_pending {
                ScreenPolicyDecision::with_outcome(
                    ScreenEligibilityNext::Stop(ScreenActionBlockReason::SystemRestorePending),
                    PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                        DecisionReasonCode::RuntimePhaseIneligible,
                        SYSTEM_RESTORE_PENDING_DETAIL,
                    )),
                )
            } else {
                ScreenPolicyDecision::new(ScreenEligibilityNext::Continue)
            }
        }
        RuntimePhaseRead::Unknown { detail } => {
            if input.system_restore_pending {
                let outcome = PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                    "runtime phase read failed; system resume restore is pending, failing closed: {detail}"
                )));
                ScreenPolicyDecision::with_outcome(
                    ScreenEligibilityNext::Stop(ScreenActionBlockReason::SystemRestorePending),
                    outcome.with_no_action(DecisionReason::with_detail(
                        DecisionReasonCode::RuntimePhaseIneligible,
                        SYSTEM_RESTORE_PENDING_DETAIL,
                    )),
                )
            } else {
                let outcome = PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                    "runtime phase read failed; failing open: {detail}"
                )));
                ScreenPolicyDecision::with_outcome(ScreenEligibilityNext::Continue, outcome)
            }
        }
    }
}

fn decide_screen_off_after_input(
    configured_input: HdmiInput,
    observation: ScreenOffInputObservation,
) -> ScreenPolicyDecision<ScreenOffNext> {
    match observation {
        ScreenOffInputObservation::Current(current_input)
            if current_input.is_hdmi(configured_input) =>
        {
            ScreenPolicyDecision::with_outcome(
                ScreenOffNext::Blank,
                PolicyOutcome::new().with_action(
                    ActionKind::TvScreenBlank,
                    DecisionReason::new(DecisionReasonCode::RuntimeEvent),
                ),
            )
        }
        ScreenOffInputObservation::Current(current_input) => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::with_detail(
                    DecisionReasonCode::InputMismatch,
                    format!(
                        "TV is on {current_input}, not {}",
                        configured_input.as_str()
                    ),
                ))
                .with_state_transition(clear_session_marker(TransitionReasonCode::InputMismatch)),
        ),
        ScreenOffInputObservation::QueryFailed(detail) => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::PowerOffFallback,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning(format!("input query failed: {detail}")))
                .with_action(
                    ActionKind::TvPowerOffFallback,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_screen_off_after_blank_result(
    observation: TvEffectObservation,
) -> ScreenPolicyDecision<ScreenOffNext> {
    match observation {
        TvEffectObservation::Succeeded => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::Stop,
            PolicyOutcome::new()
                .with_state_transition(create_session_marker(TransitionReasonCode::ActionSelected)),
        ),
        TvEffectObservation::Failed(detail) => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::PowerOffFallback,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning(format!(
                    "screen blank failed: {detail}"
                )))
                .with_action(
                    ActionKind::TvPowerOffFallback,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_screen_off_after_power_off_result(
    observation: TvEffectObservation,
) -> ScreenPolicyDecision<ScreenOffNext> {
    match observation {
        TvEffectObservation::Succeeded => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::Stop,
            PolicyOutcome::new()
                .with_state_transition(create_session_marker(TransitionReasonCode::ActionSelected)),
        ),
        TvEffectObservation::Failed(detail) => ScreenPolicyDecision::with_outcome(
            ScreenOffNext::Stop,
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "fallback power_off failed: {detail}"
            ))),
        ),
    }
}

fn decide_screen_on_start(
    policy: ScreenRestorePolicy,
    marker_exists: bool,
) -> ScreenPolicyDecision<ScreenOnNext> {
    if !marker_exists && policy != ScreenRestorePolicy::Aggressive {
        return ScreenPolicyDecision::with_outcome(
            ScreenOnNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::MarkerMissing)),
        );
    }

    let mut outcome = PolicyOutcome::new().with_action(
        ActionKind::TvScreenRestore,
        DecisionReason::new(DecisionReasonCode::RuntimeEvent),
    );
    if !marker_exists {
        outcome =
            outcome.with_diagnostic(Diagnostic::info("aggressive markerless restore requested"));
    }

    ScreenPolicyDecision::with_outcome(ScreenOnNext::Unblank, outcome)
}

fn decide_screen_on_after_unblank(
    observation: ScreenOnUnblankObservation,
) -> ScreenPolicyDecision<ScreenOnNext> {
    match observation {
        ScreenOnUnblankObservation::VerifiedActive => ScreenPolicyDecision::with_outcome(
            ScreenOnNext::Stop,
            PolicyOutcome::new().with_state_transition(clear_session_marker(
                TransitionReasonCode::RestoreCompleted,
            )),
        ),
        ScreenOnUnblankObservation::SubstateMismatch(detail) => ScreenPolicyDecision::with_outcome(
            ScreenOnNext::FullWake,
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "screen unblank rejected because the TV is not in the screen-off substate: {detail}"
            ))),
        ),
        ScreenOnUnblankObservation::Failed(detail) => ScreenPolicyDecision::with_outcome(
            ScreenOnNext::FullWake,
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "screen visibility could not be verified: {detail}"
            ))),
        ),
    }
}

fn select_screen_on_wake_packet() -> PolicyOutcome {
    PolicyOutcome::new().with_action(
        ActionKind::WakeOnLan,
        DecisionReason::new(DecisionReasonCode::RuntimeEvent),
    )
}

fn select_screen_on_input_restore_attempt() -> PolicyOutcome {
    PolicyOutcome::new().with_action(
        ActionKind::TvInputRestore,
        DecisionReason::new(DecisionReasonCode::RuntimeEvent),
    )
}

fn decide_screen_on_wake_attempt_succeeded() -> PolicyOutcome {
    PolicyOutcome::new()
        .with_state_transition(clear_session_marker(TransitionReasonCode::RestoreCompleted))
}

fn decide_screen_on_wake_exhausted(marker_exists: bool) -> PolicyOutcome {
    if marker_exists {
        PolicyOutcome::new().with_state_transition(StateTransition::preserve_marker(
            StateMarker::SessionScreenOwnership,
            TransitionReason::new(TransitionReasonCode::Other),
        ))
    } else {
        PolicyOutcome::new()
    }
}

fn apply_screen_action_eligibility<
    W: Write,
    P: RuntimePhaseProvider,
    L: SystemLifecycleStatusProvider,
>(
    writer: &mut W,
    prefix: &str,
    config: &Config,
    event: RuntimeEvent,
    phase_provider: &mut P,
    lifecycle_status: &L,
    outcome: &mut PolicyOutcome,
) -> Result<bool, RunError> {
    let lifecycle_policy_enabled = config.system_sleep_wake_policy.is_enabled();
    let session_lifecycle_action =
        is_session_screen_source(event.source) && lifecycle_policy_enabled;
    let runtime_phase = if session_lifecycle_action {
        phase_provider.machine_sleep_pending()
    } else {
        RuntimePhaseRead::NotPending
    };
    let system_restore_pending =
        session_lifecycle_action && lifecycle_status.system_restore_pending();
    let decision = decide_screen_action_eligibility(ScreenActionEligibilityInput {
        source: event.source,
        lifecycle_policy_enabled,
        runtime_phase,
        system_restore_pending,
    });

    match decision.next {
        ScreenEligibilityNext::Stop(ScreenActionBlockReason::SystemRestorePending) => {
            writeln!(
                writer,
                "{prefix}: System resume restore is pending; lifecycle owns TV actions. Skipping session screen action."
            )?
        }
        ScreenEligibilityNext::Stop(ScreenActionBlockReason::MachineSleepPending) => writeln!(
            writer,
            "{prefix}: Machine sleep is pending; lifecycle owns TV actions. Skipping session screen action."
        )?,
        ScreenEligibilityNext::Continue => {}
    }

    let should_continue = matches!(decision.next, ScreenEligibilityNext::Continue);
    outcome.merge(decision.outcome);
    Ok(should_continue)
}

fn render_screen_off_input_decision<W: Write>(
    writer: &mut W,
    configured_input: HdmiInput,
    current_input: &CurrentInput,
    decision: &ScreenPolicyDecision<ScreenOffNext>,
) -> io::Result<()> {
    match decision.next {
        ScreenOffNext::Blank => writeln!(
            writer,
            "LG Buddy Screen Off: TV is on {}. Attempting screen blank for idle...",
            configured_input.as_str()
        ),
        ScreenOffNext::Stop => writeln!(
            writer,
            "LG Buddy Screen Off: TV is on {current_input} (not {}). Skipping idle action.",
            configured_input.as_str()
        ),
        ScreenOffNext::PowerOffFallback => Ok(()),
    }
}

fn render_screen_on_start_decision<W: Write>(
    writer: &mut W,
    config: &Config,
    marker_exists: bool,
    decision: &ScreenPolicyDecision<ScreenOnNext>,
) -> io::Result<()> {
    if decision.next == ScreenOnNext::Stop {
        return writeln!(
            writer,
            "LG Buddy Screen On: State file not found. TV was not turned off by LG Buddy. Skipping wake."
        );
    }

    if !marker_exists {
        log_markerless_restore_notice(writer, "LG Buddy Screen On")?;
    }

    writeln!(
        writer,
        "LG Buddy Screen On: Turning TV on (screen wake) using input {}...",
        config.input.as_str()
    )?;
    writeln!(writer, "LG Buddy Screen On: Attempting screen unblank...")
}

fn execute_screen_blank<W: Write, C: TvClient>(
    writer: &mut W,
    _config: &Config,
    marker: &ScreenOwnershipMarker,
    tv: &TvDevice<'_, C>,
    outcome: &mut PolicyOutcome,
) -> Result<(), RunError> {
    let observation = match tv.screen().blank() {
        Ok(_) => TvEffectObservation::Succeeded,
        Err(err) => TvEffectObservation::Failed(err.to_string()),
    };
    let decision = decide_screen_off_after_blank_result(observation.clone());
    apply_screen_state_transitions(marker, &decision.outcome)?;

    match observation {
        TvEffectObservation::Succeeded => {
            writeln!(
                writer,
                "LG Buddy Screen Off: Screen blank command succeeded."
            )?;
        }
        TvEffectObservation::Failed(detail) => {
            writeln!(
                writer,
                "LG Buddy Screen Off: Screen blank failed. Falling back to power_off. {detail}"
            )?;
        }
    }

    let next = decision.next;
    outcome.merge(decision.outcome);
    if next == ScreenOffNext::PowerOffFallback {
        execute_screen_off_power_off_fallback(
            writer,
            marker,
            tv,
            ScreenOffFallbackContext::BlankFailed,
            outcome,
        )?;
    }

    Ok(())
}

fn execute_screen_off_power_off_fallback<W: Write, C: TvClient>(
    writer: &mut W,
    marker: &ScreenOwnershipMarker,
    tv: &TvDevice<'_, C>,
    context: ScreenOffFallbackContext,
    outcome: &mut PolicyOutcome,
) -> Result<(), RunError> {
    let observation = match tv.power().off() {
        Ok(_) => TvEffectObservation::Succeeded,
        Err(err) => TvEffectObservation::Failed(err.to_string()),
    };
    let decision = decide_screen_off_after_power_off_result(observation.clone());
    apply_screen_state_transitions(marker, &decision.outcome)?;

    match observation {
        TvEffectObservation::Succeeded => {
            writeln!(writer, "LG Buddy Screen Off: Fallback power_off succeeded.")?;
        }
        TvEffectObservation::Failed(detail) => match context {
            ScreenOffFallbackContext::InputQueryFailed => writeln!(
                writer,
                "LG Buddy Screen Off: Could not power off the TV (may already be off or unreachable). {detail}"
            )?,
            ScreenOffFallbackContext::BlankFailed => {
                writeln!(writer, "LG Buddy Screen Off: Fallback power_off failed. {detail}")?
            }
        },
    }

    outcome.merge(decision.outcome);
    Ok(())
}

fn execute_screen_on_unblank<W: Write, C: TvClient>(
    writer: &mut W,
    marker: &ScreenOwnershipMarker,
    tv: &TvDevice<'_, C>,
    outcome: &mut PolicyOutcome,
    anomaly: &mut ScreenRestoreAnomaly,
) -> Result<bool, RunError> {
    let initial_power_state = observe_screen_power_state(tv, anomaly, "initial");
    let (unblank_result, observation) = match initial_power_state {
        Ok(TvPowerState::Active) => (None, ScreenOnUnblankObservation::VerifiedActive),
        Ok(TvPowerState::ScreenOff) => {
            let started_at = Instant::now();
            let unblank_result = tv.screen().unblank();
            anomaly.direct_unblank = Some(TimedTvStep {
                elapsed: started_at.elapsed(),
                outcome: match &unblank_result {
                    Ok(()) => TvStepOutcome::Succeeded,
                    Err(error) => TvStepOutcome::Failed(TvFailureSnapshot::from_error(error)),
                },
            });
            let power_state = observe_screen_power_state(tv, anomaly, "after_direct_unblank");
            let observation = match &power_state {
                Ok(TvPowerState::Active) => ScreenOnUnblankObservation::VerifiedActive,
                Ok(state) => match &unblank_result {
                    Err(error) if error.indicates_screen_unblank_substate_mismatch() => {
                        ScreenOnUnblankObservation::SubstateMismatch(format!(
                            "{error}; TV verification reported `{state}`"
                        ))
                    }
                    Err(error) => ScreenOnUnblankObservation::Failed(format!(
                        "{error}; TV verification reported `{state}`"
                    )),
                    Ok(()) => ScreenOnUnblankObservation::Failed(format!(
                        "screen unblank was acknowledged but TV verification reported `{state}`"
                    )),
                },
                Err(verification_error) => match &unblank_result {
                    Err(error) if error.indicates_screen_unblank_substate_mismatch() => {
                        ScreenOnUnblankObservation::SubstateMismatch(format!(
                            "{error}; TV verification failed: {verification_error}"
                        ))
                    }
                    Err(error) => ScreenOnUnblankObservation::Failed(format!(
                        "{error}; TV verification failed: {verification_error}"
                    )),
                    Ok(()) => ScreenOnUnblankObservation::Failed(format!(
                        "screen unblank was acknowledged but TV verification failed: {verification_error}"
                    )),
                },
            };
            (Some(unblank_result), observation)
        }
        Ok(state) => (
            None,
            ScreenOnUnblankObservation::Failed(format!(
                "TV verification reported `{state}` before screen unblank"
            )),
        ),
        Err(error) => (
            None,
            ScreenOnUnblankObservation::Failed(format!(
                "could not observe TV power state before screen unblank: {error}"
            )),
        ),
    };
    let decision = decide_screen_on_after_unblank(observation.clone());
    apply_screen_state_transitions(marker, &decision.outcome)?;

    match observation {
        ScreenOnUnblankObservation::VerifiedActive => {
            if matches!(unblank_result, Some(Ok(()))) {
                writeln!(
                    writer,
                    "LG Buddy Screen On: Screen unblank succeeded. Verified TV power state Active. Clearing wake state."
                )?;
            } else {
                writeln!(
                    writer,
                    "LG Buddy Screen On: Verified TV power state Active. Clearing wake state."
                )?;
            }
        }
        ScreenOnUnblankObservation::SubstateMismatch(_) => {
            writeln!(
                writer,
                "LG Buddy Screen On: TV rejected screen unblank because it is not in the screen-off substate. Falling back to full wake."
            )?;
        }
        ScreenOnUnblankObservation::Failed(_) => {}
    }

    let next = decision.next;
    outcome.merge(decision.outcome);
    match next {
        ScreenOnNext::Stop => Ok(true),
        ScreenOnNext::FullWake => Ok(false),
        ScreenOnNext::Unblank => unreachable!("unblank cannot select itself"),
    }
}

fn execute_screen_on_full_wake<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    deps: ScreenOnWakeDeps<'_, C, S, Sl>,
    outcome: &mut PolicyOutcome,
    anomaly: &mut ScreenRestoreAnomaly,
) -> Result<(), RunError> {
    writeln!(
        writer,
        "LG Buddy Screen On: Screen visibility could not be verified. Falling back to full wake."
    )?;
    writeln!(
        writer,
        "LG Buddy Screen On: Sending initial Wake-on-LAN packet..."
    )?;
    outcome.merge(select_screen_on_wake_packet());
    anomaly.wake_packets.push(send_wake_packet(
        writer,
        "LG Buddy Screen On",
        deps.tv,
        deps.wol_sender,
        &config.tv_mac,
        outcome,
    )?);
    deps.sleeper.sleep(screen_on_initial_wake_delay());

    for attempt in 1..=SCREEN_ON_WAKE_ATTEMPTS {
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt}: setting input to {}...",
            config.input.as_str()
        )?;

        outcome.merge(select_screen_on_input_restore_attempt());
        let input_started_at = Instant::now();
        let input_result = deps.tv.input().set(config.input);
        anomaly.input_attempts.push(TimedTvStep {
            elapsed: input_started_at.elapsed(),
            outcome: match &input_result {
                Ok(()) => TvStepOutcome::Succeeded,
                Err(error) => TvStepOutcome::Failed(TvFailureSnapshot::from_error(error)),
            },
        });

        let power_state =
            observe_screen_power_state(deps.tv, anomaly, format!("after_input_attempt_{attempt}"));
        if matches!(power_state, Ok(TvPowerState::Active)) {
            let success_outcome = decide_screen_on_wake_attempt_succeeded();
            apply_screen_state_transitions(deps.marker, &success_outcome)?;
            writeln!(
                writer,
                "LG Buddy Screen On: Wake attempt {attempt} succeeded. Verified TV power state Active. Clearing wake state."
            )?;
            outcome.merge(success_outcome);
            return Ok(());
        }

        if matches!(power_state, Ok(TvPowerState::ScreenOff)) {
            if input_result.is_ok() {
                writeln!(
                    writer,
                    "LG Buddy Screen On: Fallback input acknowledgement left the TV in Screen Off; reconciling screen visibility."
                )?;
            } else {
                writeln!(
                    writer,
                    "LG Buddy Screen On: Fallback input attempt failed and the TV remains in Screen Off; reconciling screen visibility."
                )?;
            }
            let unblank_started_at = Instant::now();
            let unblank_result = deps.tv.screen().unblank();
            anomaly.recovery_unblank_attempts.push(TimedTvStep {
                elapsed: unblank_started_at.elapsed(),
                outcome: match &unblank_result {
                    Ok(()) => TvStepOutcome::Succeeded,
                    Err(error) => TvStepOutcome::Failed(TvFailureSnapshot::from_error(error)),
                },
            });
            let reconciled_power_state = observe_screen_power_state(
                deps.tv,
                anomaly,
                format!("after_recovery_unblank_attempt_{attempt}"),
            );
            if matches!(reconciled_power_state, Ok(TvPowerState::Active)) {
                let success_outcome = decide_screen_on_wake_attempt_succeeded();
                apply_screen_state_transitions(deps.marker, &success_outcome)?;
                writeln!(
                    writer,
                    "LG Buddy Screen On: Wake attempt {attempt} succeeded. Verified TV power state Active. Clearing wake state."
                )?;
                outcome.merge(success_outcome);
                return Ok(());
            }
        }

        let retry_delay = screen_on_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt} failed. Resending WoL and retrying in {}s...",
            retry_delay.as_secs()
        )?;
        outcome.merge(select_screen_on_wake_packet());
        anomaly.wake_packets.push(send_wake_packet(
            writer,
            "LG Buddy Screen On",
            deps.tv,
            deps.wol_sender,
            &config.tv_mac,
            outcome,
        )?);
        deps.sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Screen On: Wake failed after {SCREEN_ON_WAKE_ATTEMPTS} attempts. LG Buddy will retry on the next restore event."
    )?;
    let exhausted_outcome = decide_screen_on_wake_exhausted(deps.marker_exists);
    apply_screen_state_transitions(deps.marker, &exhausted_outcome)?;
    outcome.merge(exhausted_outcome);
    Err(RunError::Policy(format!(
        "screen-on wake sequence failed after {SCREEN_ON_WAKE_ATTEMPTS} attempts"
    )))
}

fn log_markerless_restore_notice<W: Write>(writer: &mut W, prefix: &str) -> io::Result<()> {
    writeln!(
        writer,
        "{prefix}: State file not found. Aggressive restore policy is enabled, so LG Buddy will attempt wake anyway."
    )
}

fn send_wake_packet<W: Write, C: TvClient, S: WakeOnLanSender>(
    writer: &mut W,
    prefix: &str,
    tv: &TvDevice<'_, C>,
    wol_sender: &S,
    tv_mac: &MacAddress,
    outcome: &mut PolicyOutcome,
) -> Result<WakePacketSnapshot, RunError> {
    let started_at = Instant::now();
    let error = match tv.power().wake(wol_sender, tv_mac) {
        Ok(()) => None,
        Err(err) => {
            let detail = compact_failure_detail(&err.to_string());
            outcome.diagnostics.push(Diagnostic::warning(format!(
                "Wake-on-LAN send failed: {err}"
            )));
            writeln!(
                writer,
                "{prefix}: Wake-on-LAN send failed. Continuing anyway. {err}"
            )?;
            Some(detail)
        }
    };

    Ok(WakePacketSnapshot {
        elapsed: started_at.elapsed(),
        error,
    })
}

fn observe_screen_power_state<C: TvClient>(
    tv: &TvDevice<'_, C>,
    anomaly: &mut ScreenRestoreAnomaly,
    stage: impl Into<String>,
) -> Result<TvPowerState, TvError> {
    let started_at = Instant::now();
    let result = tv.power().state();
    let snapshot_result = match &result {
        Ok(state) => Ok(state.clone()),
        Err(error) => Err(TvFailureSnapshot::from_error(error)),
    };
    anomaly
        .power_state_observations
        .push(TimedPowerStateObservation {
            stage: stage.into(),
            elapsed: started_at.elapsed(),
            result: snapshot_result,
        });
    result
}

fn render_tv_step(step: &TimedTvStep) -> String {
    match &step.outcome {
        TvStepOutcome::Succeeded => {
            format!("succeeded elapsed_ms={}", step.elapsed.as_millis())
        }
        TvStepOutcome::Failed(failure) => format!(
            "failed kind={} elapsed_ms={} detail={:?}",
            failure.kind.as_str(),
            step.elapsed.as_millis(),
            failure.detail,
        ),
    }
}

fn render_tv_steps(steps: &[TimedTvStep]) -> String {
    if steps.is_empty() {
        return "none".to_string();
    }

    steps
        .iter()
        .enumerate()
        .map(|(index, step)| format!("attempt_{}=[{}]", index + 1, render_tv_step(step)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_wake_packets(packets: &[WakePacketSnapshot]) -> String {
    if packets.is_empty() {
        return "none".to_string();
    }

    packets
        .iter()
        .enumerate()
        .map(|(index, packet)| match &packet.error {
            None => format!(
                "attempt_{}=[sent elapsed_ms={}]",
                index + 1,
                packet.elapsed.as_millis()
            ),
            Some(error) => format!(
                "attempt_{}=[failed elapsed_ms={} detail={error:?}]",
                index + 1,
                packet.elapsed.as_millis()
            ),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_power_state_observation(observation: &TimedPowerStateObservation) -> String {
    match &observation.result {
        Ok(state) => format!(
            "state={:?} elapsed_ms={}",
            state.to_string(),
            observation.elapsed.as_millis()
        ),
        Err(failure) => format!(
            "failed kind={} elapsed_ms={} detail={:?}",
            failure.kind.as_str(),
            observation.elapsed.as_millis(),
            failure.detail,
        ),
    }
}

fn render_power_state_observations(observations: &[TimedPowerStateObservation]) -> String {
    if observations.is_empty() {
        return "none".to_string();
    }

    observations
        .iter()
        .map(|observation| {
            format!(
                "{}=[{}]",
                observation.stage,
                render_power_state_observation(observation)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_failure_detail(detail: &str) -> String {
    detail
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("no detail")
        .to_string()
}

fn event_source_label(source: EventSource) -> &'static str {
    match source {
        EventSource::CliApi => "cli-api",
        EventSource::LinuxLogind => "linux-logind",
        EventSource::LinuxNetworkManager => "linux-network-manager",
        EventSource::LinuxSystemd => "linux-systemd",
        EventSource::DesktopSession => "desktop-session",
        EventSource::AuxiliaryInput => "auxiliary-input",
        EventSource::FuturePlatform => "future-platform",
    }
}

fn marker_state(exists: bool) -> &'static str {
    if exists {
        "present"
    } else {
        "absent"
    }
}

fn is_session_screen_source(source: EventSource) -> bool {
    matches!(
        source,
        EventSource::DesktopSession | EventSource::AuxiliaryInput
    )
}

fn apply_screen_state_transitions(
    marker: &ScreenOwnershipMarker,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    for transition in &outcome.state_transitions {
        match transition {
            StateTransition::CreateMarker {
                marker: StateMarker::SessionScreenOwnership,
                ..
            } => marker.create()?,
            StateTransition::ClearMarker {
                marker: StateMarker::SessionScreenOwnership,
                ..
            } => marker.clear()?,
            StateTransition::PreserveMarker {
                marker: StateMarker::SessionScreenOwnership,
                ..
            } => {}
            StateTransition::CreateMarker { .. }
            | StateTransition::ClearMarker { .. }
            | StateTransition::PreserveMarker { .. } => {
                return Err(RunError::Policy(
                    "screen policy emitted a non-session marker transition".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn create_session_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::create_marker(
        StateMarker::SessionScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn clear_session_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::clear_marker(
        StateMarker::SessionScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn duration_override_secs(env_key: &str, default: Duration) -> Duration {
    env::var(env_key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

fn screen_on_initial_wake_delay() -> Duration {
    duration_override_secs(
        "LG_BUDDY_SCREEN_ON_INITIAL_WAKE_DELAY_SECS",
        SCREEN_ON_INITIAL_WAKE_DELAY,
    )
}

fn screen_on_retry_delay(attempt: u32) -> Duration {
    duration_override_secs(
        "LG_BUDDY_SCREEN_ON_RETRY_DELAY_SECS",
        Duration::from_secs(u64::from((attempt * 2).min(30))),
    )
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        decide_screen_action_eligibility, decide_screen_off_after_input,
        decide_screen_on_after_unblank, decide_screen_on_start, run_screen_off_with_outcome,
        run_screen_off_with_outcome_for_event, run_screen_on_with_outcome,
        run_screen_on_with_outcome_for_event, NoopSystemLifecycleStatusProvider,
        ScreenActionBlockReason, ScreenActionEligibilityInput, ScreenEligibilityNext,
        ScreenOffInputObservation, ScreenOffNext, ScreenOnDeps, ScreenOnNext,
        ScreenOnUnblankObservation, Sleeper, SystemLifecycleStatusProvider,
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
    use crate::runtime_phase::{RuntimePhaseProvider, RuntimePhaseRead};
    use crate::state::ScreenOwnershipMarker;
    use crate::tv::{BscpylgtvCommandClient, CurrentInput};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use std::cell::RefCell;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};
    use support::MockBscpylgtv;

    #[test]
    fn pure_policy_marks_session_action_ineligible_while_sleep_is_pending() {
        let decision = decide_screen_action_eligibility(ScreenActionEligibilityInput {
            source: EventSource::DesktopSession,
            lifecycle_policy_enabled: true,
            runtime_phase: RuntimePhaseRead::Pending,
            system_restore_pending: false,
        });

        assert_eq!(
            decision.next,
            ScreenEligibilityNext::Stop(ScreenActionBlockReason::MachineSleepPending)
        );
        assert!(decision.outcome.actions.is_empty());
        assert!(decision.outcome.state_transitions.is_empty());
        assert_eq!(decision.outcome.no_actions.len(), 1);
        assert_eq!(
            decision.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
    }

    #[test]
    fn pure_policy_fails_open_when_runtime_phase_is_unknown() {
        let decision = decide_screen_action_eligibility(ScreenActionEligibilityInput {
            source: EventSource::DesktopSession,
            lifecycle_policy_enabled: true,
            runtime_phase: RuntimePhaseRead::Unknown {
                detail: "system bus unavailable".to_string(),
            },
            system_restore_pending: false,
        });

        assert_eq!(decision.next, ScreenEligibilityNext::Continue);
        assert!(decision.outcome.actions.is_empty());
        assert!(decision.outcome.no_actions.is_empty());
        assert_eq!(decision.outcome.diagnostics.len(), 1);
        assert!(decision.outcome.diagnostics[0]
            .message
            .contains("failing open"));
    }

    #[test]
    fn pure_policy_fails_closed_when_runtime_phase_is_unknown_but_system_restore_is_pending() {
        let decision = decide_screen_action_eligibility(ScreenActionEligibilityInput {
            source: EventSource::DesktopSession,
            lifecycle_policy_enabled: true,
            runtime_phase: RuntimePhaseRead::Unknown {
                detail: "system bus unavailable".to_string(),
            },
            system_restore_pending: true,
        });

        assert_eq!(
            decision.next,
            ScreenEligibilityNext::Stop(ScreenActionBlockReason::SystemRestorePending)
        );
        assert!(decision.outcome.actions.is_empty());
        assert_eq!(decision.outcome.no_actions.len(), 1);
        assert_eq!(decision.outcome.diagnostics.len(), 1);
        assert!(decision.outcome.diagnostics[0]
            .message
            .contains("failing closed"));
    }

    #[test]
    fn pure_policy_marks_session_action_ineligible_while_system_restore_is_pending() {
        let decision = decide_screen_action_eligibility(ScreenActionEligibilityInput {
            source: EventSource::DesktopSession,
            lifecycle_policy_enabled: true,
            runtime_phase: RuntimePhaseRead::NotPending,
            system_restore_pending: true,
        });

        assert_eq!(
            decision.next,
            ScreenEligibilityNext::Stop(ScreenActionBlockReason::SystemRestorePending)
        );
        assert!(decision.outcome.actions.is_empty());
        assert!(decision.outcome.state_transitions.is_empty());
        assert_eq!(decision.outcome.no_actions.len(), 1);
        assert_eq!(
            decision.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
    }

    #[test]
    fn pure_screen_off_policy_selects_blank_or_marker_clear_from_input_observation() {
        let blank = decide_screen_off_after_input(
            HdmiInput::Hdmi2,
            ScreenOffInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi2)),
        );

        assert_eq!(blank.next, ScreenOffNext::Blank);
        assert_eq!(blank.outcome.actions[0].kind, ActionKind::TvScreenBlank);
        assert!(blank.outcome.state_transitions.is_empty());

        let skip = decide_screen_off_after_input(
            HdmiInput::Hdmi2,
            ScreenOffInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi4)),
        );

        assert_eq!(skip.next, ScreenOffNext::Stop);
        assert_eq!(
            skip.outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );
        assert_eq!(
            skip.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::InputMismatch),
            )]
        );
    }

    #[test]
    fn pure_screen_on_policy_applies_restore_policy_without_reading_marker_state_itself() {
        let conservative_missing = decide_screen_on_start(ScreenRestorePolicy::MarkerOnly, false);

        assert_eq!(conservative_missing.next, ScreenOnNext::Stop);
        assert_eq!(
            conservative_missing.outcome.no_actions[0].reason.code,
            DecisionReasonCode::MarkerMissing
        );

        let aggressive_missing = decide_screen_on_start(ScreenRestorePolicy::Aggressive, false);

        assert_eq!(aggressive_missing.next, ScreenOnNext::Unblank);
        assert_eq!(
            aggressive_missing.outcome.actions[0].kind,
            ActionKind::TvScreenRestore
        );
        assert_eq!(aggressive_missing.outcome.diagnostics.len(), 1);
    }

    #[test]
    fn pure_screen_on_policy_does_not_claim_an_unattempted_unblank_failed() {
        let decision = decide_screen_on_after_unblank(ScreenOnUnblankObservation::Failed(
            "could not observe the initial TV power state".to_string(),
        ));

        assert_eq!(decision.next, ScreenOnNext::FullWake);
        assert_eq!(
            decision.outcome.diagnostics[0].message,
            "screen visibility could not be verified: could not observe the initial TV power state"
        );
    }

    #[test]
    fn screen_off_outcome_records_blank_action_and_marker_creation() {
        let temp_dir = TestDir::new("screen-outcome-off-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-outcome-off-success-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvScreenBlank]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(outcome.diagnostics.is_empty());
        assert!(marker.exists());
    }

    #[test]
    fn screen_off_outcome_records_input_mismatch_no_action_and_marker_clear() {
        let temp_dir = TestDir::new("screen-outcome-off-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale marker");
        let mock = MockBscpylgtv::new("screen-outcome-off-skip-tv");
        mock.set_input("HDMI_4");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off skip should succeed");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::InputMismatch),
            )]
        );
        assert!(!marker.exists());
    }

    #[test]
    fn screen_on_outcome_records_restore_action_and_marker_clear() {
        let temp_dir = TestDir::new("screen-outcome-on-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-outcome-on-success-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = run_screen_on_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("screen-on should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvScreenRestore]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::RestoreCompleted),
            )]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(!marker.exists());
        assert!(!rendered(&output).contains("LG Buddy Screen Restore Anomaly:"));
    }

    #[test]
    fn screen_on_fallback_reconciles_false_success_and_records_the_full_state_sequence() {
        let temp_dir = TestDir::new("screen-on-false-success-snapshot");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-false-success-snapshot-tv");
        mock.set_screen_on(false);
        mock.queue_error(
            "turn_screen_on",
            7,
            "native-like unblank transport failure\n",
        );
        mock.queue_set_input_ack_without_screen_on();
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("reconciler should continue after the false acknowledgement");

        let output = rendered(&output);
        assert!(output.contains("LG Buddy Screen Restore Anomaly:"));
        assert!(output.contains(
            "context: source=cli-api platform=bscpylgtv configured_input=HDMI_2 marker_before=present"
        ));
        assert!(output.contains("direct_unblank: failed kind=rejected"));
        assert!(output.contains("native-like unblank transport failure"));
        assert!(output.contains("wake_packets: attempt_1=[sent"));
        assert!(output.contains("input_attempts: attempt_1=[succeeded"));
        assert!(output.contains("recovery_unblank_attempts: attempt_1=[succeeded"));
        assert!(output.contains("initial=[state=\"Screen Off\""));
        assert!(output.contains("after_direct_unblank=[state=\"Screen Off\""));
        assert!(output.contains("after_input_attempt_1=[state=\"Screen Off\""));
        assert!(output.contains("after_recovery_unblank_attempt_1=[state=\"Active\""));
        assert!(
            output.contains("internal_outcome: reconciliation=verified_active marker_after=absent")
        );
        assert!(!marker.exists());
        assert!(mock.state_snapshot().screen_on);
        assert_call_commands(
            &mock,
            &[
                "get_power_state",
                "turn_screen_on",
                "get_power_state",
                "set_input",
                "get_power_state",
                "turn_screen_on",
                "get_power_state",
            ],
        );
    }

    #[test]
    fn screen_on_outcome_records_marker_missing_no_action() {
        let temp_dir = TestDir::new("screen-outcome-on-missing-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-outcome-on-missing-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = run_screen_on_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("missing marker should skip");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::MarkerMissing
        );
        assert!(outcome.state_transitions.is_empty());
    }

    #[test]
    fn session_screen_off_is_ineligible_while_machine_sleep_is_pending() {
        let temp_dir = TestDir::new("screen-phase-off-pending");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-phase-off-pending-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut phase = FixedRuntimePhaseProvider::pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome_for_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionIdle),
            &mut phase,
            &lifecycle_status,
        )
        .expect("pending machine sleep should skip session screen off");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert!(outcome.state_transitions.is_empty());
        assert!(!marker.exists());
        assert!(mock.calls().is_empty());
        assert!(rendered(&output).contains("Machine sleep is pending"));
    }

    #[test]
    fn session_screen_on_is_ineligible_while_machine_sleep_is_pending() {
        let temp_dir = TestDir::new("screen-phase-on-pending");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-phase-on-pending-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let mut phase = FixedRuntimePhaseProvider::pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let mut output = Vec::new();
        let outcome = run_screen_on_with_outcome_for_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            ScreenOnDeps {
                tv_client: &client,
                wol_sender: &wol,
                sleeper: &sleeper,
                phase_provider: &mut phase,
                lifecycle_status: &lifecycle_status,
            },
            RuntimeEvent::new(
                EventSource::AuxiliaryInput,
                RuntimeEventKind::UserActivityObserved,
            ),
        )
        .expect("pending machine sleep should skip session screen on");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert!(outcome.state_transitions.is_empty());
        assert!(marker.exists());
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn session_screen_on_is_ineligible_while_system_restore_is_pending() {
        let temp_dir = TestDir::new("screen-phase-on-system-restore-pending");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-phase-on-system-restore-pending-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let mut phase = FixedRuntimePhaseProvider::not_pending();
        let lifecycle_status = FixedSystemLifecycleStatusProvider { pending: true };

        let mut output = Vec::new();
        let outcome = run_screen_on_with_outcome_for_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            ScreenOnDeps {
                tv_client: &client,
                wol_sender: &wol,
                sleeper: &sleeper,
                phase_provider: &mut phase,
                lifecycle_status: &lifecycle_status,
            },
            RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::ScreenWakeRequested,
            ),
        )
        .expect("pending system restore should skip session screen on");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert!(outcome.state_transitions.is_empty());
        assert!(marker.exists());
        assert!(mock.calls().is_empty());
        assert!(wol.calls.borrow().is_empty());
        assert!(rendered(&output).contains("System resume restore is pending"));
    }

    #[test]
    fn disabled_lifecycle_policy_does_not_block_session_screen_actions() {
        let temp_dir = TestDir::new("screen-phase-disabled");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-phase-disabled-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut config = sample_config(HdmiInput::Hdmi2);
        config.system_sleep_wake_policy = SystemSleepWakePolicy::Disabled;
        let mut phase = FixedRuntimePhaseProvider::pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome_for_event(
            &mut output,
            &config,
            &marker,
            &client,
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionIdle),
            &mut phase,
            &lifecycle_status,
        )
        .expect("disabled lifecycle policy should fail open");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvScreenBlank]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off"]);
    }

    #[test]
    fn unknown_runtime_phase_fails_open_for_session_screen_actions() {
        let temp_dir = TestDir::new("screen-phase-unknown");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-phase-unknown-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut phase = FixedRuntimePhaseProvider::unknown("system bus unavailable");
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome_for_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionIdle),
            &mut phase,
            &lifecycle_status,
        )
        .expect("unknown runtime phase should fail open");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvScreenBlank]
        );
        assert_eq!(outcome.diagnostics.len(), 1);
        assert!(outcome.no_actions.is_empty());
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off"]);
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

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
        }
    }

    struct FixedRuntimePhaseProvider {
        read: RuntimePhaseRead,
    }

    impl FixedRuntimePhaseProvider {
        fn pending() -> Self {
            Self {
                read: RuntimePhaseRead::Pending,
            }
        }

        fn not_pending() -> Self {
            Self {
                read: RuntimePhaseRead::NotPending,
            }
        }

        fn unknown(detail: &str) -> Self {
            Self {
                read: RuntimePhaseRead::Unknown {
                    detail: detail.to_string(),
                },
            }
        }
    }

    impl RuntimePhaseProvider for FixedRuntimePhaseProvider {
        fn machine_sleep_pending(&mut self) -> RuntimePhaseRead {
            self.read.clone()
        }
    }

    struct FixedSystemLifecycleStatusProvider {
        pending: bool,
    }

    impl SystemLifecycleStatusProvider for FixedSystemLifecycleStatusProvider {
        fn system_restore_pending(&self) -> bool {
            self.pending
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
