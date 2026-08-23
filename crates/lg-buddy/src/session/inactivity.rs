use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityObservation {
    DesktopActivityObserved,
    ProviderActive,
    WakeRequested,
    UserActivityObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityDecision {
    BlankNow,
    RestoreNow,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InactivityPhase {
    Unknown,
    Active,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactivityEngine {
    blank_after: Duration,
    blank_at: Instant,
    phase: InactivityPhase,
    restore_pending: bool,
}

impl InactivityEngine {
    pub fn new(blank_after: Duration, started_at: Instant) -> Self {
        Self {
            blank_after,
            blank_at: started_at + blank_after,
            phase: InactivityPhase::Unknown,
            restore_pending: false,
        }
    }

    pub fn new_with_restore_pending(blank_after: Duration, started_at: Instant) -> Self {
        Self {
            blank_after,
            blank_at: started_at + blank_after,
            phase: InactivityPhase::Unknown,
            restore_pending: true,
        }
    }

    pub fn time_until_blank(&self, now: Instant) -> Option<Duration> {
        (self.phase != InactivityPhase::Idle).then(|| self.blank_at.saturating_duration_since(now))
    }

    pub fn observe_activity(
        &mut self,
        observation: InactivityObservation,
        observed_at: Instant,
    ) -> InactivityDecision {
        self.blank_at = self.blank_at.max(observed_at + self.blank_after);

        match self.phase {
            InactivityPhase::Idle => {
                self.phase = InactivityPhase::Active;
                self.restore_pending = false;
                InactivityDecision::RestoreNow
            }
            InactivityPhase::Unknown => {
                self.phase = InactivityPhase::Active;
                if self.restore_pending
                    || observation != InactivityObservation::DesktopActivityObserved
                {
                    self.restore_pending = false;
                    InactivityDecision::RestoreNow
                } else {
                    InactivityDecision::NoOp
                }
            }
            InactivityPhase::Active => InactivityDecision::NoOp,
        }
    }

    pub fn observe_time(&mut self, observed_at: Instant) -> InactivityDecision {
        if self.phase == InactivityPhase::Idle || observed_at < self.blank_at {
            return InactivityDecision::NoOp;
        }

        self.phase = InactivityPhase::Idle;
        InactivityDecision::BlankNow
    }
}

#[cfg(test)]
mod tests {
    use super::{InactivityDecision, InactivityEngine, InactivityObservation};
    use std::time::{Duration, Instant};

    fn test_engine(started_at: Instant) -> InactivityEngine {
        InactivityEngine::new(Duration::from_secs(5), started_at)
    }

    #[test]
    fn timeout_blanks_once() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.observe_time(started_at + Duration::from_millis(4_999)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(6)),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn desktop_activity_resets_the_timeout() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_secs(4),
            ),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(9)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn owned_blank_restores_on_first_desktop_activity_after_restart() {
        let started_at = Instant::now();
        let mut engine =
            InactivityEngine::new_with_restore_pending(Duration::from_secs(5), started_at);

        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_secs(1),
            ),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn pending_restore_does_not_suppress_timeout_reconciliation() {
        let started_at = Instant::now();
        let mut engine =
            InactivityEngine::new_with_restore_pending(Duration::from_secs(5), started_at);

        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn auxiliary_activity_resets_the_timeout_and_restores_after_blank() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(6),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(10)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(11)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn wake_and_provider_activity_reset_the_same_timeout() {
        let started_at = Instant::now();

        for observation in [
            InactivityObservation::ProviderActive,
            InactivityObservation::WakeRequested,
        ] {
            let mut engine = test_engine(started_at);
            assert_eq!(
                engine.observe_activity(observation, started_at + Duration::from_secs(4)),
                InactivityDecision::RestoreNow
            );
            assert_eq!(
                engine.observe_time(started_at + Duration::from_secs(5)),
                InactivityDecision::NoOp
            );
            assert_eq!(
                engine.observe_time(started_at + Duration::from_secs(9)),
                InactivityDecision::BlankNow
            );
        }
    }

    #[test]
    fn stale_activity_cannot_move_the_deadline_back() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(4),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(2),
            ),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(7)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(9)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn idle_phase_has_no_pending_timeout() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.time_until_blank(started_at),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        assert_eq!(engine.time_until_blank(started_at), None);
    }
}
