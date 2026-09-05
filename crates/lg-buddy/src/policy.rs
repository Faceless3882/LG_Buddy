#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyOutcome {
    pub actions: Vec<ActionDecision>,
    pub no_actions: Vec<NoActionDecision>,
    pub state_transitions: Vec<StateTransition>,
    pub diagnostics: Vec<Diagnostic>,
}

impl PolicyOutcome {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_action(mut self, kind: ActionKind, reason: DecisionReason) -> Self {
        self.actions.push(ActionDecision::new(kind, reason));
        self
    }

    pub fn with_no_action(mut self, reason: DecisionReason) -> Self {
        self.no_actions.push(NoActionDecision::new(reason));
        self
    }

    pub fn with_state_transition(mut self, transition: StateTransition) -> Self {
        self.state_transitions.push(transition);
        self
    }

    pub fn with_diagnostic(mut self, diagnostic: Diagnostic) -> Self {
        self.diagnostics.push(diagnostic);
        self
    }

    pub fn merge(&mut self, other: Self) {
        self.actions.extend(other.actions);
        self.no_actions.extend(other.no_actions);
        self.state_transitions.extend(other.state_transitions);
        self.diagnostics.extend(other.diagnostics);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecision {
    pub kind: ActionKind,
    pub reason: DecisionReason,
}

impl ActionDecision {
    pub const fn new(kind: ActionKind, reason: DecisionReason) -> Self {
        Self { kind, reason }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoActionDecision {
    pub reason: DecisionReason,
}

impl NoActionDecision {
    pub const fn new(reason: DecisionReason) -> Self {
        Self { reason }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    TvScreenBlank,
    TvScreenRestore,
    TvPowerOffFallback,
    TvTimedIdlePowerOff,
    TvInputRestore,
    WakeOnLan,
    TvSystemSleepPowerOff,
    TvSystemResumeRestore,
    TvStartupRestore,
    TvShutdownPowerOff,
    BrightnessUi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionReason {
    pub code: DecisionReasonCode,
    pub detail: Option<String>,
}

impl DecisionReason {
    pub const fn new(code: DecisionReasonCode) -> Self {
        Self { code, detail: None }
    }

    pub fn with_detail(code: DecisionReasonCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReasonCode {
    RuntimeEvent,
    ManualRequest,
    ConfigDisabled,
    InputMismatch,
    MarkerMissing,
    RestorePolicyDenied,
    DuplicateSystemSleepAttempt,
    RuntimePhaseIneligible,
    RuntimePhaseUnknown,
    TransportFailure,
    NotApplicable,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateTransition {
    CreateMarker {
        marker: StateMarker,
        reason: TransitionReason,
    },
    ClearMarker {
        marker: StateMarker,
        reason: TransitionReason,
    },
    PreserveMarker {
        marker: StateMarker,
        reason: TransitionReason,
    },
}

impl StateTransition {
    pub const fn create_marker(marker: StateMarker, reason: TransitionReason) -> Self {
        Self::CreateMarker { marker, reason }
    }

    pub const fn clear_marker(marker: StateMarker, reason: TransitionReason) -> Self {
        Self::ClearMarker { marker, reason }
    }

    pub const fn preserve_marker(marker: StateMarker, reason: TransitionReason) -> Self {
        Self::PreserveMarker { marker, reason }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMarker {
    SessionScreenOwnership,
    SystemScreenOwnership,
    SystemSleepAttempt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionReason {
    pub code: TransitionReasonCode,
    pub detail: Option<String>,
}

impl TransitionReason {
    pub const fn new(code: TransitionReasonCode) -> Self {
        Self { code, detail: None }
    }

    pub fn with_detail(code: TransitionReasonCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReasonCode {
    ActionSelected,
    RestoreCompleted,
    ConfigDisabled,
    InputMismatch,
    MarkerMissing,
    DuplicateSystemSleepAttempt,
    RuntimePhaseIneligible,
    TransportFailure,
    StartupBoot,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
}

impl Diagnostic {
    pub fn new(level: DiagnosticLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(DiagnosticLevel::Info, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(DiagnosticLevel::Warning, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(DiagnosticLevel::Error, message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[cfg(test)]
mod tests {
    use super::{
        ActionDecision, ActionKind, DecisionReason, DecisionReasonCode, Diagnostic,
        DiagnosticLevel, NoActionDecision, PolicyOutcome, StateMarker, StateTransition,
        TransitionReason, TransitionReasonCode,
    };

    #[test]
    fn outcome_records_action_and_state_transition_together() {
        let outcome = PolicyOutcome::new()
            .with_action(
                ActionKind::TvScreenBlank,
                DecisionReason::new(DecisionReasonCode::RuntimeEvent),
            )
            .with_state_transition(StateTransition::create_marker(
                StateMarker::SessionScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            ));

        assert_eq!(
            outcome.actions,
            vec![ActionDecision::new(
                ActionKind::TvScreenBlank,
                DecisionReason::new(DecisionReasonCode::RuntimeEvent),
            )]
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
    }

    #[test]
    fn outcome_records_explicit_no_action_with_reason() {
        let outcome = PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
            DecisionReasonCode::RuntimePhaseIneligible,
            "machine sleep owns TV actions",
        ));

        assert_eq!(
            outcome.no_actions,
            vec![NoActionDecision::new(DecisionReason::with_detail(
                DecisionReasonCode::RuntimePhaseIneligible,
                "machine sleep owns TV actions",
            ))]
        );
        assert!(outcome.actions.is_empty());
        assert!(outcome.state_transitions.is_empty());
    }

    #[test]
    fn state_transitions_cover_marker_create_clear_and_preserve() {
        let reason = TransitionReason::with_detail(
            TransitionReasonCode::DuplicateSystemSleepAttempt,
            "already handled this sleep cycle",
        );

        let outcome = PolicyOutcome::new()
            .with_state_transition(StateTransition::create_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            ))
            .with_state_transition(StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::RestoreCompleted),
            ))
            .with_state_transition(StateTransition::preserve_marker(
                StateMarker::SystemSleepAttempt,
                reason.clone(),
            ));

        assert_eq!(
            outcome.state_transitions,
            vec![
                StateTransition::create_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
                StateTransition::clear_marker(
                    StateMarker::SystemScreenOwnership,
                    TransitionReason::new(TransitionReasonCode::RestoreCompleted),
                ),
                StateTransition::preserve_marker(StateMarker::SystemSleepAttempt, reason),
            ]
        );
    }

    #[test]
    fn outcome_records_diagnostics_separately_from_policy_decisions() {
        let outcome = PolicyOutcome::new()
            .with_no_action(DecisionReason::new(DecisionReasonCode::TransportFailure))
            .with_diagnostic(Diagnostic::warning(
                "logind phase read failed; failing open",
            ))
            .with_diagnostic(Diagnostic::error("TV command failed"));

        assert_eq!(
            outcome.diagnostics,
            vec![
                Diagnostic::new(
                    DiagnosticLevel::Warning,
                    "logind phase read failed; failing open",
                ),
                Diagnostic::new(DiagnosticLevel::Error, "TV command failed"),
            ]
        );
        assert_eq!(
            outcome.no_actions,
            vec![NoActionDecision::new(DecisionReason::new(
                DecisionReasonCode::TransportFailure,
            ))]
        );
    }

    #[test]
    fn outcomes_can_be_merged_without_losing_trail_order() {
        let mut left = PolicyOutcome::new()
            .with_action(
                ActionKind::TvSystemSleepPowerOff,
                DecisionReason::new(DecisionReasonCode::RuntimeEvent),
            )
            .with_state_transition(StateTransition::create_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            ));
        let right = PolicyOutcome::new()
            .with_state_transition(StateTransition::create_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            ))
            .with_diagnostic(Diagnostic::info("system sleep attempt recorded"));

        left.merge(right);

        assert_eq!(
            left.actions,
            vec![ActionDecision::new(
                ActionKind::TvSystemSleepPowerOff,
                DecisionReason::new(DecisionReasonCode::RuntimeEvent),
            )]
        );
        assert_eq!(
            left.state_transitions,
            vec![
                StateTransition::create_marker(
                    StateMarker::SystemScreenOwnership,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
                StateTransition::create_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
            ]
        );
        assert_eq!(
            left.diagnostics,
            vec![Diagnostic::info("system sleep attempt recorded")]
        );
    }
}
