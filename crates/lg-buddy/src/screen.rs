use std::env;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{
    load_config, resolve_config_path_from_env, Config, HdmiInput, MacAddress, ScreenRestorePolicy,
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
use crate::tv::{build_tv_client, CurrentInput, TvClient, TvClientBuildOptions, TvDevice, TvError};
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
    Succeeded,
    Failed(String),
}

#[derive(Debug)]
struct ScreenRestoreTrace {
    started_at: Instant,
    entries: Vec<String>,
    failed: bool,
}

impl ScreenRestoreTrace {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            entries: Vec::new(),
            failed: false,
        }
    }

    fn record(&mut self, operation: impl AsRef<str>, result: &Result<(), TvError>) {
        let elapsed_ms = self.started_at.elapsed().as_millis();
        match result {
            Ok(()) => self.entries.push(format!(
                "{}=succeeded elapsed_ms={elapsed_ms}",
                operation.as_ref()
            )),
            Err(error) => {
                self.failed = true;
                self.entries.push(format!(
                    "{}=failed kind={} elapsed_ms={elapsed_ms} detail={:?}",
                    operation.as_ref(),
                    error.kind().as_str(),
                    compact_failure_detail(error.detail())
                ));
            }
        }
    }

    fn render_if_failed<W: Write>(
        &self,
        writer: &mut W,
        event: RuntimeEvent,
        config: &Config,
        marker_before: bool,
        marker_after: bool,
    ) -> io::Result<()> {
        if !self.failed {
            return Ok(());
        }

        writeln!(writer, "LG Buddy Screen Restore Failure Context:")?;
        writeln!(
            writer,
            "  context: source={} platform={} configured_input={} marker_before={}",
            event_source_label(event.source),
            config.tv_platform,
            config.input.as_str(),
            marker_state(marker_before),
        )?;
        writeln!(writer, "  operations: {}", self.entries.join(" | "))?;
        writeln!(
            writer,
            "  marker_after={} total_elapsed_ms={}",
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
    run_screen_off_from_env_for_event_with_result(writer, event).map(|_| ())
}

#[derive(Debug)]
pub(crate) struct ScreenOffActionResult {
    pub(crate) blank_succeeded: bool,
}

pub(crate) fn run_screen_off_from_env_for_event_with_result<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<ScreenOffActionResult, RunError> {
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

    run_screen_off_with_result_for_event(
        writer,
        &config,
        &marker,
        &tv_client,
        event,
        &mut phase_provider,
        &lifecycle_status,
    )
    .map(|result| ScreenOffActionResult {
        blank_succeeded: result.blank_succeeded,
    })
}

pub(crate) fn run_timed_power_off_from_env_for_event<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    if !config.screen_idle_blank.is_enabled() {
        writeln!(
            writer,
            "LG Buddy Timed Power Off: Screen idle blanking is disabled; skipping power-off."
        )?;
        return Ok(());
    }
    if !marker.exists() {
        writeln!(
            writer,
            "LG Buddy Timed Power Off: Screen ownership marker is absent; skipping power-off."
        )?;
        return Ok(());
    }
    let tv_client = build_tv_client(
        &config_path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production(),
    )?;
    let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
    let lifecycle_status =
        SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

    run_timed_power_off_with_event(
        writer,
        &config,
        &marker,
        &tv_client,
        event,
        &mut phase_provider,
        &lifecycle_status,
    )
    .map(|_| ())
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

#[cfg(test)]
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

#[cfg(test)]
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
    run_screen_off_with_result_for_event(
        writer,
        config,
        marker,
        tv_client,
        event,
        phase_provider,
        lifecycle_status,
    )
    .map(|result| result.outcome)
}

struct ScreenOffExecutionResult {
    #[cfg(test)]
    outcome: PolicyOutcome,
    blank_succeeded: bool,
}

fn run_screen_off_with_result_for_event<
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
) -> Result<ScreenOffExecutionResult, RunError> {
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
        return Ok(ScreenOffExecutionResult {
            #[cfg(test)]
            outcome,
            blank_succeeded: false,
        });
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);
    let mut blank_succeeded = false;

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
                blank_succeeded = execute_screen_blank(writer, config, marker, &tv, &mut outcome)?;
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

    Ok(ScreenOffExecutionResult {
        #[cfg(test)]
        outcome,
        blank_succeeded,
    })
}

#[cfg(test)]
pub(crate) fn run_timed_power_off_with<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
) -> Result<PolicyOutcome, RunError> {
    let mut phase_provider = NoopRuntimePhaseProvider;
    let lifecycle_status = NoopSystemLifecycleStatusProvider;
    run_timed_power_off_with_event(
        writer,
        config,
        marker,
        tv_client,
        RuntimeEvent::new(
            EventSource::DesktopSession,
            RuntimeEventKind::SessionTimedPowerOff,
        ),
        &mut phase_provider,
        &lifecycle_status,
    )
}

pub(crate) fn run_timed_power_off_with_event<
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
    const PREFIX: &str = "LG Buddy Timed Power Off";

    if !config.screen_idle_blank.is_enabled() {
        writeln!(
            writer,
            "{PREFIX}: Screen idle blanking is disabled; skipping power-off."
        )?;
        return Ok(PolicyOutcome::new()
            .with_no_action(DecisionReason::new(DecisionReasonCode::ConfigDisabled)));
    }

    let mut outcome = PolicyOutcome::new();
    if !apply_screen_action_eligibility(
        writer,
        PREFIX,
        config,
        event,
        phase_provider,
        lifecycle_status,
        &mut outcome,
    )? {
        return Ok(outcome);
    }

    if !marker.exists() {
        writeln!(
            writer,
            "{PREFIX}: Screen ownership marker is absent; skipping power-off."
        )?;
        outcome.merge(
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::MarkerMissing)),
        );
        return Ok(outcome);
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);
    let current_input = match tv.input().current() {
        Ok(input) => input,
        Err(err) => {
            writeln!(
                writer,
                "{PREFIX}: Could not query TV input; skipping power-off without a blind fallback. {err}"
            )?;
            outcome.merge(
                PolicyOutcome::new()
                    .with_no_action(DecisionReason::with_detail(
                        DecisionReasonCode::TransportFailure,
                        err.to_string(),
                    ))
                    .with_diagnostic(Diagnostic::warning(format!(
                        "timed power-off input query failed: {err}"
                    ))),
            );
            return Ok(outcome);
        }
    };

    if !current_input.is_hdmi(config.input) {
        let mismatch = PolicyOutcome::new()
            .with_no_action(DecisionReason::with_detail(
                DecisionReasonCode::InputMismatch,
                format!("TV is on {current_input}, not {}", config.input.as_str()),
            ))
            .with_state_transition(clear_session_marker(TransitionReasonCode::InputMismatch));
        apply_screen_state_transitions(marker, &mismatch)?;
        outcome.merge(mismatch);
        writeln!(
            writer,
            "{PREFIX}: TV is on {current_input} (not {}); ownership was lost, so power-off was skipped and the marker cleared.",
            config.input.as_str()
        )?;
        return Ok(outcome);
    }

    outcome.merge(
        PolicyOutcome::new()
            .with_action(
                ActionKind::TvTimedIdlePowerOff,
                DecisionReason::new(DecisionReasonCode::RuntimeEvent),
            )
            .with_state_transition(StateTransition::preserve_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )),
    );
    writeln!(
        writer,
        "{PREFIX}: Ownership, input, and lifecycle checks passed; powering off TV."
    )?;
    match tv.power().off() {
        Ok(()) => writeln!(
            writer,
            "{PREFIX}: TV power-off succeeded; preserving the ownership marker for activity restore."
        )?,
        Err(err) => {
            writeln!(
                writer,
                "{PREFIX}: TV power-off failed; preserving the ownership marker. {err}"
            )?;
            outcome.merge(PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "timed TV power-off failed: {err}"
            ))));
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
    let start_decision = decide_screen_on_start(config.screen_restore_policy, marker_exists);
    render_screen_on_start_decision(writer, config, marker_exists, &start_decision)?;
    let next = start_decision.next;
    outcome.merge(start_decision.outcome);
    if next == ScreenOnNext::Stop {
        return Ok(outcome);
    }

    let tv = TvDevice::new(deps.tv_client, config.tv_ip);
    let mut trace = ScreenRestoreTrace::new();
    if next == ScreenOnNext::Unblank
        && execute_screen_on_unblank(writer, marker, &tv, &mut outcome, &mut trace)?
    {
        trace.render_if_failed(writer, event, config, marker_exists, marker.exists())?;
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
        &mut trace,
    );
    trace.render_if_failed(writer, event, config, marker_exists, marker.exists())?;
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
        ScreenOnUnblankObservation::Succeeded => ScreenPolicyDecision::with_outcome(
            ScreenOnNext::Stop,
            PolicyOutcome::new().with_state_transition(clear_session_marker(
                TransitionReasonCode::RestoreCompleted,
            )),
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
) -> Result<bool, RunError> {
    let observation = match tv.screen().blank() {
        Ok(_) => TvEffectObservation::Succeeded,
        Err(err) => TvEffectObservation::Failed(err.to_string()),
    };
    let blank_succeeded = matches!(observation, TvEffectObservation::Succeeded);
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

    Ok(blank_succeeded)
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
    trace: &mut ScreenRestoreTrace,
) -> Result<bool, RunError> {
    let result = tv.screen().unblank();
    trace.record("direct_unblank", &result);
    let observation = match result {
        Ok(()) => ScreenOnUnblankObservation::Succeeded,
        Err(error) => ScreenOnUnblankObservation::Failed(error.to_string()),
    };
    let decision = decide_screen_on_after_unblank(observation.clone());
    apply_screen_state_transitions(marker, &decision.outcome)?;

    match observation {
        ScreenOnUnblankObservation::Succeeded => {
            writeln!(
                writer,
                "LG Buddy Screen On: Screen unblank succeeded. Clearing wake state."
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
    trace: &mut ScreenRestoreTrace,
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
    send_wake_packet(
        writer,
        "LG Buddy Screen On",
        deps.tv,
        deps.wol_sender,
        &config.tv_mac,
        outcome,
    )?;
    deps.sleeper.sleep(screen_on_initial_wake_delay());

    for attempt in 1..=SCREEN_ON_WAKE_ATTEMPTS {
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt}: setting input to {}...",
            config.input.as_str()
        )?;

        outcome.merge(select_screen_on_input_restore_attempt());
        if attempt_input_restore(deps.tv, config.input, attempt, trace).is_ok() {
            let success_outcome = decide_screen_on_wake_attempt_succeeded();
            apply_screen_state_transitions(deps.marker, &success_outcome)?;
            writeln!(
                writer,
                "LG Buddy Screen On: Wake attempt {attempt} succeeded. Clearing wake state."
            )?;
            outcome.merge(success_outcome);
            return Ok(());
        }

        let retry_delay = screen_on_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt} failed. Resending WoL and retrying in {}s...",
            retry_delay.as_secs()
        )?;
        outcome.merge(select_screen_on_wake_packet());
        send_wake_packet(
            writer,
            "LG Buddy Screen On",
            deps.tv,
            deps.wol_sender,
            &config.tv_mac,
            outcome,
        )?;
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

fn attempt_input_restore<C: TvClient>(
    tv: &TvDevice<'_, C>,
    input: HdmiInput,
    attempt: u32,
    trace: &mut ScreenRestoreTrace,
) -> Result<(), TvError> {
    let result = tv.input().set(input);
    trace.record(format!("input_attempt_{attempt}"), &result);

    let Err(error) = result else {
        return Ok(());
    };
    if !error.indicates_screen_not_visible() {
        return Err(error);
    }

    let unblank_result = tv.screen().unblank();
    trace.record(format!("recovery_unblank_{attempt}"), &unblank_result);
    unblank_result?;

    let retry_result = tv.input().set(input);
    trace.record(format!("input_retry_{attempt}"), &retry_result);
    retry_result
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
) -> Result<(), RunError> {
    if let Err(err) = tv.power().wake(wol_sender, tv_mac) {
        outcome.diagnostics.push(Diagnostic::warning(format!(
            "Wake-on-LAN send failed: {err}"
        )));
        writeln!(
            writer,
            "{prefix}: Wake-on-LAN send failed. Continuing anyway. {err}"
        )?;
    }

    Ok(())
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
        EventSource::DesktopSession | EventSource::AuxiliaryInput | EventSource::LinuxLogind
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
        decide_screen_action_eligibility, decide_screen_off_after_input, decide_screen_on_start,
        run_screen_off_with_outcome, run_screen_off_with_outcome_for_event,
        run_screen_off_with_result_for_event, run_screen_on_with_outcome,
        run_screen_on_with_outcome_for_event, run_timed_power_off_with,
        run_timed_power_off_with_event, NoopSystemLifecycleStatusProvider, ScreenActionBlockReason,
        ScreenActionEligibilityInput, ScreenEligibilityNext, ScreenOffInputObservation,
        ScreenOffNext, ScreenOnDeps, ScreenOnNext, Sleeper, SystemLifecycleStatusProvider,
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
    fn logind_lock_uses_the_existing_input_blank_and_marker_policy() {
        let temp_dir = TestDir::new("screen-outcome-logind-lock");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-outcome-logind-lock-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut phase = FixedRuntimePhaseProvider::not_pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let mut output = Vec::new();
        let outcome = run_screen_off_with_outcome_for_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            RuntimeEvent::new(EventSource::LinuxLogind, RuntimeEventKind::SessionLocked),
            &mut phase,
            &lifecycle_status,
        )
        .expect("logind lock should use normal session blank policy");

        assert_eq!(outcome.actions.len(), 1);
        assert_eq!(outcome.actions[0].kind, ActionKind::TvScreenBlank);
        assert!(marker.exists());
    }

    #[test]
    fn screen_off_result_arms_escalation_only_for_an_actual_blank() {
        let temp_dir = TestDir::new("screen-off-actual-blank-result");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-actual-blank-result-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("turn_screen_off", 1, "blank failed\n");
        let client = client_for_mock(&mock);
        let mut phase = FixedRuntimePhaseProvider::not_pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;

        let result = run_screen_off_with_result_for_event(
            &mut Vec::new(),
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionIdle),
            &mut phase,
            &lifecycle_status,
        )
        .expect("fallback power-off should complete");

        assert!(!result.blank_succeeded);
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off", "power_off"]);
    }

    #[test]
    fn timed_power_off_rechecks_input_and_preserves_marker_after_success() {
        let temp_dir = TestDir::new("timed-power-off-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-success-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut output = Vec::new();

        let outcome = run_timed_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("timed power-off should complete");

        assert!(marker.exists());
        assert_eq!(outcome.actions.len(), 1);
        assert_eq!(outcome.actions[0].kind, ActionKind::TvTimedIdlePowerOff);
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("TV power-off succeeded"));
    }

    #[test]
    fn timed_power_off_requires_the_ownership_marker() {
        let temp_dir = TestDir::new("timed-power-off-marker-missing");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("timed-power-off-marker-missing-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut output = Vec::new();

        let outcome = run_timed_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("missing marker should skip safely");

        assert!(mock.calls().is_empty());
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::MarkerMissing
        );
        assert!(rendered(&output).contains("ownership marker is absent"));
    }

    #[test]
    fn timed_power_off_respects_disabled_idle_blanking() {
        let temp_dir = TestDir::new("timed-power-off-disabled");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-disabled-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut config = sample_config(HdmiInput::Hdmi2);
        config.screen_idle_blank = ScreenIdleBlankPolicy::Disabled;
        let mut output = Vec::new();

        let outcome = run_timed_power_off_with(&mut output, &config, &marker, &client)
            .expect("disabled idle blanking should skip safely");

        assert!(mock.calls().is_empty());
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::ConfigDisabled
        );
        assert!(rendered(&output).contains("idle blanking is disabled"));
    }

    #[test]
    fn timed_power_off_clears_ownership_on_input_mismatch() {
        let temp_dir = TestDir::new("timed-power-off-input-mismatch");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-input-mismatch-tv");
        mock.set_input("HDMI_4");
        let client = client_for_mock(&mock);
        let mut output = Vec::new();

        run_timed_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("input mismatch should skip safely");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("ownership was lost"));
    }

    #[test]
    fn timed_power_off_does_not_fall_back_when_input_query_fails() {
        let temp_dir = TestDir::new("timed-power-off-input-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-input-failure-tv");
        mock.queue_error("get_input", 1, "query failed\n");
        let client = client_for_mock(&mock);
        let mut output = Vec::new();

        run_timed_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("input query failure should skip safely");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("without a blind fallback"));
    }

    #[test]
    fn timed_power_off_rechecks_lifecycle_eligibility() {
        let temp_dir = TestDir::new("timed-power-off-lifecycle-pending");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-lifecycle-pending-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let mut phase = FixedRuntimePhaseProvider::pending();
        let lifecycle_status = NoopSystemLifecycleStatusProvider;
        let mut output = Vec::new();

        run_timed_power_off_with_event(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::SessionTimedPowerOff,
            ),
            &mut phase,
            &lifecycle_status,
        )
        .expect("pending lifecycle should skip timed power-off");

        assert!(marker.exists());
        assert!(mock.calls().is_empty());
        assert!(rendered(&output).contains("Machine sleep is pending"));
    }

    #[test]
    fn timed_power_off_attempt_failure_preserves_marker_without_retrying_here() {
        let temp_dir = TestDir::new("timed-power-off-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create ownership marker");
        let mock = MockBscpylgtv::new("timed-power-off-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "power failed\n");
        let client = client_for_mock(&mock);
        let mut output = Vec::new();

        run_timed_power_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("power failure should remain a completed attempt");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("TV power-off failed"));
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
        assert!(!rendered(&output).contains("LG Buddy Screen Restore Failure Context:"));
    }

    #[test]
    fn legacy_screen_on_reconciles_input_acknowledged_while_screen_off() {
        let temp_dir = TestDir::new("legacy-screen-on-false-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("legacy-screen-on-false-success-tv");
        mock.set_screen_on(false);
        mock.queue_error("turn_screen_on", 1, "unblank transport failure\n");
        mock.queue_set_input_ack_without_screen_on();
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("legacy adapter should reconcile screen visibility");

        assert!(!marker.exists());
        assert!(mock.state_snapshot().screen_on);
        assert_call_commands(
            &mock,
            &[
                "turn_screen_on",
                "get_power_state",
                "set_input",
                "get_power_state",
                "turn_screen_on",
                "get_power_state",
                "set_input",
                "get_power_state",
            ],
        );
        let output = rendered(&output);
        assert!(output.contains("LG Buddy Screen Restore Failure Context:"));
        assert!(output.contains("direct_unblank=failed kind=screen_not_visible"));
        assert!(output.contains("input_attempt_1=failed kind=screen_not_visible"));
        assert!(output.contains("recovery_unblank_1=succeeded"));
        assert!(output.contains("input_retry_1=succeeded"));
        assert!(output.contains("marker_after=absent"));
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
    fn logind_lock_screen_off_is_ineligible_while_machine_sleep_is_pending() {
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
            RuntimeEvent::new(EventSource::LinuxLogind, RuntimeEventKind::SessionLocked),
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
