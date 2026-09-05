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
    session_locked_since: Option<Instant>,
    lock_activity_floor: Option<Instant>,
    latest_independent_activity_at: Option<Instant>,
}

impl InactivityEngine {
    pub fn new(blank_after: Duration, started_at: Instant) -> Self {
        Self {
            blank_after,
            blank_at: started_at + blank_after,
            phase: InactivityPhase::Unknown,
            restore_pending: false,
            session_locked_since: None,
            lock_activity_floor: None,
            latest_independent_activity_at: None,
        }
    }

    pub fn new_with_restore_pending(blank_after: Duration, started_at: Instant) -> Self {
        Self {
            blank_after,
            blank_at: started_at + blank_after,
            phase: InactivityPhase::Unknown,
            restore_pending: true,
            session_locked_since: None,
            lock_activity_floor: None,
            latest_independent_activity_at: None,
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
        if matches!(
            observation,
            InactivityObservation::DesktopActivityObserved
                | InactivityObservation::UserActivityObserved
        ) {
            self.latest_independent_activity_at = Some(
                self.latest_independent_activity_at
                    .map_or(observed_at, |latest| latest.max(observed_at)),
            );
        }

        if self.session_locked_since.is_some()
            && matches!(
                observation,
                InactivityObservation::ProviderActive | InactivityObservation::WakeRequested
            )
        {
            return InactivityDecision::NoOp;
        }

        if self.phase == InactivityPhase::Idle
            && self.lock_activity_floor.is_some_and(|locked_at| {
                !matches!(
                    observation,
                    InactivityObservation::DesktopActivityObserved
                        | InactivityObservation::UserActivityObserved
                ) || observed_at <= locked_at
            })
        {
            return InactivityDecision::NoOp;
        }

        self.blank_at = self.blank_at.max(observed_at + self.blank_after);

        match self.phase {
            InactivityPhase::Idle => {
                self.phase = InactivityPhase::Active;
                self.restore_pending = false;
                self.lock_activity_floor = None;
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
        self.lock_activity_floor = self.session_locked_since;
        InactivityDecision::BlankNow
    }

    pub fn observe_lock(&mut self, observed_at: Instant) -> InactivityDecision {
        let locked_at = *self.session_locked_since.get_or_insert(observed_at);
        if self
            .latest_independent_activity_at
            .is_some_and(|activity_at| activity_at > locked_at)
        {
            self.lock_activity_floor = None;
            return InactivityDecision::NoOp;
        }
        self.lock_activity_floor = Some(locked_at);

        if self.phase == InactivityPhase::Idle {
            return InactivityDecision::NoOp;
        }

        self.phase = InactivityPhase::Idle;
        InactivityDecision::BlankNow
    }

    pub fn observe_unlock(&mut self) {
        self.session_locked_since = None;
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

    #[test]
    fn lock_blanks_immediately_once_and_activity_can_restore() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);
        let locked_at = started_at + Duration::from_secs(1);

        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::NoOp);
        assert_eq!(engine.time_until_blank(started_at), None);
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_secs(2),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(7)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn lock_restore_requires_independent_activity_newer_than_the_lock() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);
        let mut engine = test_engine(started_at);

        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_millis(900),
            ),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_secs(2),
            ),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn newer_independent_activity_processed_before_lock_keeps_the_screen_active() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);
        let activity_at = started_at + Duration::from_secs(2);
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.observe_activity(InactivityObservation::DesktopActivityObserved, activity_at,),
            InactivityDecision::NoOp
        );
        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::NoOp);
        assert_eq!(
            engine.time_until_blank(activity_at),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn provider_unlock_signals_do_not_restore_a_lock_blank() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);
        let mut engine = test_engine(started_at);

        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        for observation in [
            InactivityObservation::WakeRequested,
            InactivityObservation::ProviderActive,
        ] {
            assert_eq!(
                engine.observe_activity(observation, started_at + Duration::from_secs(2)),
                InactivityDecision::NoOp
            );
        }

        engine.observe_unlock();
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::ProviderActive,
                started_at + Duration::from_secs(3),
            ),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(3),
            ),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn timeout_while_locked_keeps_the_independent_activity_gate() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);
        let mut engine = test_engine(started_at);

        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                started_at + Duration::from_secs(2),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(7)),
            InactivityDecision::BlankNow
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::WakeRequested,
                started_at + Duration::from_secs(8),
            ),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(8),
            ),
            InactivityDecision::RestoreNow
        );
    }
}
