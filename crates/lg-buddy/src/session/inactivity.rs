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
    TimedPowerOffNow,
    RestoreNow,
    NoOp,
}

/// Ignore independent input briefly after a lock-triggered blank so input from
/// disengaging from the machine cannot immediately undo the blank.
pub(crate) const POST_LOCK_ACTIVITY_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InactivityPhase {
    Unknown,
    Active,
    BlankRequested,
    Blanked,
    InactiveWithoutEscalation,
    TimedPowerOffAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactivityEngine {
    blank_after: Duration,
    blank_at: Option<Instant>,
    provider_driven_blank: bool,
    power_off_after: Duration,
    power_off_at: Option<Instant>,
    phase: InactivityPhase,
    restore_pending: bool,
    session_locked_since: Option<Instant>,
    lock_activity_floor: Option<Instant>,
    post_lock_activity_grace_until: Option<Instant>,
    latest_independent_activity_at: Option<Instant>,
}

impl InactivityEngine {
    pub const DEFAULT_POWER_OFF_AFTER: Duration = Duration::from_secs(5 * 60);

    pub fn new(blank_after: Duration, started_at: Instant) -> Self {
        Self::new_with_power_off_after(blank_after, Self::DEFAULT_POWER_OFF_AFTER, started_at)
    }

    pub fn new_provider_driven(power_off_after: Duration, started_at: Instant) -> Self {
        let mut engine =
            Self::new_with_power_off_after(Duration::ZERO, power_off_after, started_at);
        engine.blank_at = None;
        engine.provider_driven_blank = true;
        engine
    }

    pub fn new_provider_driven_with_restore_pending(
        power_off_after: Duration,
        started_at: Instant,
    ) -> Self {
        let mut engine = Self::new_with_restore_pending_and_power_off_after(
            Duration::ZERO,
            power_off_after,
            started_at,
        );
        engine.blank_at = None;
        engine.provider_driven_blank = true;
        engine
    }

    pub fn new_with_power_off_after(
        blank_after: Duration,
        power_off_after: Duration,
        started_at: Instant,
    ) -> Self {
        Self {
            blank_after,
            blank_at: Some(started_at + blank_after),
            provider_driven_blank: false,
            power_off_after,
            power_off_at: None,
            phase: InactivityPhase::Unknown,
            restore_pending: false,
            session_locked_since: None,
            lock_activity_floor: None,
            post_lock_activity_grace_until: None,
            latest_independent_activity_at: None,
        }
    }

    pub fn new_with_restore_pending(blank_after: Duration, started_at: Instant) -> Self {
        Self::new_with_restore_pending_and_power_off_after(
            blank_after,
            Self::DEFAULT_POWER_OFF_AFTER,
            started_at,
        )
    }

    pub fn new_with_restore_pending_and_power_off_after(
        blank_after: Duration,
        power_off_after: Duration,
        started_at: Instant,
    ) -> Self {
        Self {
            blank_after,
            blank_at: Some(started_at + blank_after),
            provider_driven_blank: false,
            power_off_after,
            power_off_at: Some(started_at + power_off_after),
            phase: InactivityPhase::Blanked,
            restore_pending: true,
            session_locked_since: None,
            lock_activity_floor: None,
            post_lock_activity_grace_until: None,
            latest_independent_activity_at: None,
        }
    }

    pub fn time_until_action(&self, now: Instant) -> Option<Duration> {
        match self.phase {
            InactivityPhase::Unknown | InactivityPhase::Active => self
                .blank_at
                .map(|deadline| deadline.saturating_duration_since(now)),
            InactivityPhase::Blanked => self
                .power_off_at
                .map(|deadline| deadline.saturating_duration_since(now)),
            InactivityPhase::BlankRequested
            | InactivityPhase::InactiveWithoutEscalation
            | InactivityPhase::TimedPowerOffAttempted => None,
        }
    }

    pub fn timed_power_off_pending(&self) -> bool {
        self.phase == InactivityPhase::Blanked && self.power_off_at.is_some()
    }

    pub fn observe_provider_idle(&mut self) -> InactivityDecision {
        if matches!(
            self.phase,
            InactivityPhase::Unknown | InactivityPhase::Active
        ) {
            self.phase = InactivityPhase::BlankRequested;
            self.blank_at = None;
            return InactivityDecision::BlankNow;
        }

        InactivityDecision::NoOp
    }

    pub fn complete_blank(&mut self, succeeded: bool, completed_at: Instant) {
        if self.phase != InactivityPhase::BlankRequested {
            return;
        }

        if succeeded {
            self.phase = InactivityPhase::Blanked;
            self.restore_pending = true;
            self.power_off_at = Some(completed_at + self.power_off_after);
        } else {
            self.phase = InactivityPhase::InactiveWithoutEscalation;
            self.power_off_at = None;
        }
    }

    pub fn observe_activity(
        &mut self,
        observation: InactivityObservation,
        observed_at: Instant,
    ) -> InactivityDecision {
        let independent_activity = matches!(
            observation,
            InactivityObservation::DesktopActivityObserved
                | InactivityObservation::UserActivityObserved
        );
        if independent_activity {
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

        let inactive = matches!(
            self.phase,
            InactivityPhase::BlankRequested
                | InactivityPhase::Blanked
                | InactivityPhase::InactiveWithoutEscalation
                | InactivityPhase::TimedPowerOffAttempted
        );
        if inactive
            && independent_activity
            && self
                .post_lock_activity_grace_until
                .is_some_and(|grace_until| observed_at < grace_until)
        {
            return InactivityDecision::NoOp;
        }

        if inactive
            && self
                .lock_activity_floor
                .is_some_and(|locked_at| !independent_activity || observed_at <= locked_at)
        {
            return InactivityDecision::NoOp;
        }

        if self.provider_driven_blank {
            self.blank_at = None;
        } else {
            self.blank_at = Some(
                self.blank_at
                    .unwrap_or(observed_at)
                    .max(observed_at + self.blank_after),
            );
        }

        match self.phase {
            InactivityPhase::BlankRequested
            | InactivityPhase::Blanked
            | InactivityPhase::InactiveWithoutEscalation
            | InactivityPhase::TimedPowerOffAttempted => {
                self.phase = InactivityPhase::Active;
                self.restore_pending = false;
                self.power_off_at = None;
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
        match self.phase {
            InactivityPhase::Unknown | InactivityPhase::Active
                if self
                    .blank_at
                    .is_some_and(|deadline| observed_at >= deadline) =>
            {
                self.phase = InactivityPhase::BlankRequested;
                self.lock_activity_floor = self.session_locked_since;
                InactivityDecision::BlankNow
            }
            InactivityPhase::Blanked
                if self
                    .power_off_at
                    .is_some_and(|deadline| observed_at >= deadline) =>
            {
                self.phase = InactivityPhase::TimedPowerOffAttempted;
                self.power_off_at = None;
                InactivityDecision::TimedPowerOffNow
            }
            _ => InactivityDecision::NoOp,
        }
    }

    pub fn observe_lock(&mut self, observed_at: Instant) -> InactivityDecision {
        if self.session_locked_since.is_some() {
            return InactivityDecision::NoOp;
        }

        self.session_locked_since = Some(observed_at);
        let grace_until = observed_at + POST_LOCK_ACTIVITY_GRACE;
        if self
            .latest_independent_activity_at
            .is_some_and(|activity_at| activity_at >= grace_until)
        {
            self.lock_activity_floor = None;
            self.post_lock_activity_grace_until = None;
            return InactivityDecision::NoOp;
        }
        self.lock_activity_floor = Some(observed_at);

        if matches!(
            self.phase,
            InactivityPhase::BlankRequested
                | InactivityPhase::Blanked
                | InactivityPhase::InactiveWithoutEscalation
                | InactivityPhase::TimedPowerOffAttempted
        ) {
            return InactivityDecision::NoOp;
        }

        self.post_lock_activity_grace_until = Some(grace_until);
        self.phase = InactivityPhase::BlankRequested;
        self.blank_at = None;
        InactivityDecision::BlankNow
    }

    pub fn observe_unlock(&mut self) {
        self.session_locked_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InactivityDecision, InactivityEngine, InactivityObservation, POST_LOCK_ACTIVITY_GRACE,
    };
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
    fn restart_with_owned_blank_starts_a_fresh_power_off_grace_period() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_with_restore_pending_and_power_off_after(
            Duration::from_secs(5),
            Duration::from_secs(5),
            started_at,
        );

        assert_eq!(
            engine.observe_time(started_at + Duration::from_millis(4_999)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::TimedPowerOffNow
        );
    }

    #[test]
    fn successful_blank_schedules_one_power_off_from_completion_time() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_with_power_off_after(
            Duration::from_secs(5),
            Duration::from_secs(5),
            started_at,
        );

        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        engine.complete_blank(true, started_at + Duration::from_secs(7));
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(11)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(12)),
            InactivityDecision::TimedPowerOffNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(20)),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn failed_or_skipped_blank_never_schedules_power_off() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_with_power_off_after(
            Duration::from_secs(5),
            Duration::from_secs(5),
            started_at,
        );

        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        engine.complete_blank(false, started_at + Duration::from_secs(5));
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(30)),
            InactivityDecision::NoOp
        );
        assert!(!engine.timed_power_off_pending());
    }

    #[test]
    fn activity_at_power_off_deadline_cancels_before_timeout_is_observed() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_with_power_off_after(
            Duration::from_secs(5),
            Duration::from_secs(5),
            started_at,
        );
        let deadline = started_at + Duration::from_secs(10);

        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        engine.complete_blank(true, started_at + Duration::from_secs(5));
        assert_eq!(
            engine.observe_activity(InactivityObservation::UserActivityObserved, deadline),
            InactivityDecision::RestoreNow
        );
        assert_eq!(engine.observe_time(deadline), InactivityDecision::NoOp);
    }

    #[test]
    fn provider_driven_idle_uses_the_same_post_blank_policy() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_provider_driven(Duration::from_secs(5), started_at);

        assert_eq!(engine.time_until_action(started_at), None);
        assert_eq!(engine.observe_provider_idle(), InactivityDecision::BlankNow);
        engine.complete_blank(true, started_at);
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::TimedPowerOffNow
        );
    }

    #[test]
    fn provider_driven_activity_waits_for_the_next_provider_idle() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_provider_driven(Duration::from_secs(5), started_at);

        assert_eq!(engine.observe_provider_idle(), InactivityDecision::BlankNow);
        engine.complete_blank(true, started_at);
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::ProviderActive,
                started_at + Duration::from_secs(1),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(30)),
            InactivityDecision::NoOp
        );
        assert_eq!(engine.observe_provider_idle(), InactivityDecision::BlankNow);
    }

    #[test]
    fn fresh_activity_allows_one_attempt_in_the_next_idle_cycle() {
        let started_at = Instant::now();
        let mut engine = InactivityEngine::new_with_power_off_after(
            Duration::from_secs(1),
            Duration::from_secs(1),
            started_at,
        );

        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(1)),
            InactivityDecision::BlankNow
        );
        engine.complete_blank(true, started_at + Duration::from_secs(1));
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(2)),
            InactivityDecision::TimedPowerOffNow
        );
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::UserActivityObserved,
                started_at + Duration::from_secs(3),
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(4)),
            InactivityDecision::BlankNow
        );
        engine.complete_blank(true, started_at + Duration::from_secs(4));
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::TimedPowerOffNow
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
            engine.time_until_action(started_at),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(5)),
            InactivityDecision::BlankNow
        );
        assert_eq!(engine.time_until_action(started_at), None);
    }

    #[test]
    fn lock_blanks_immediately_once_and_activity_at_the_grace_boundary_can_restore() {
        let started_at = Instant::now();
        let mut engine = test_engine(started_at);
        let locked_at = started_at + Duration::from_secs(1);

        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::NoOp);
        assert_eq!(engine.time_until_action(started_at), None);
        assert_eq!(
            engine.observe_activity(
                InactivityObservation::DesktopActivityObserved,
                locked_at + POST_LOCK_ACTIVITY_GRACE,
            ),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe_time(started_at + Duration::from_secs(7)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn lock_grace_suppresses_pre_lock_and_immediate_independent_activity() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);

        for observation in [
            InactivityObservation::DesktopActivityObserved,
            InactivityObservation::UserActivityObserved,
        ] {
            let mut engine = test_engine(started_at);
            assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
            engine.complete_blank(true, locked_at);

            for observed_at in [
                locked_at - Duration::from_millis(1),
                locked_at + Duration::from_millis(1),
            ] {
                assert_eq!(
                    engine.observe_activity(observation, observed_at),
                    InactivityDecision::NoOp
                );
                assert!(engine.timed_power_off_pending());
            }
        }
    }

    #[test]
    fn lock_grace_accepts_independent_activity_at_and_after_the_boundary() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);

        for observation in [
            InactivityObservation::DesktopActivityObserved,
            InactivityObservation::UserActivityObserved,
        ] {
            for observed_at in [
                locked_at + POST_LOCK_ACTIVITY_GRACE,
                locked_at + POST_LOCK_ACTIVITY_GRACE + Duration::from_millis(1),
            ] {
                let mut engine = test_engine(started_at);
                assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
                engine.complete_blank(true, locked_at);

                assert_eq!(
                    engine.observe_activity(observation, observed_at),
                    InactivityDecision::RestoreNow
                );
                assert!(!engine.timed_power_off_pending());
            }
        }
    }

    #[test]
    fn activity_inside_the_grace_processed_before_lock_does_not_prevent_blanking() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);

        for observation in [
            InactivityObservation::DesktopActivityObserved,
            InactivityObservation::UserActivityObserved,
        ] {
            let mut engine = test_engine(started_at);
            assert_eq!(
                engine.observe_activity(
                    observation,
                    locked_at + POST_LOCK_ACTIVITY_GRACE - Duration::from_millis(1),
                ),
                if observation == InactivityObservation::DesktopActivityObserved {
                    InactivityDecision::NoOp
                } else {
                    InactivityDecision::RestoreNow
                }
            );
            assert_eq!(engine.observe_lock(locked_at), InactivityDecision::BlankNow);
        }
    }

    #[test]
    fn activity_at_the_grace_boundary_processed_before_lock_keeps_the_screen_active() {
        let started_at = Instant::now();
        let locked_at = started_at + Duration::from_secs(1);
        let activity_at = locked_at + POST_LOCK_ACTIVITY_GRACE;
        let mut engine = test_engine(started_at);

        assert_eq!(
            engine.observe_activity(InactivityObservation::DesktopActivityObserved, activity_at,),
            InactivityDecision::NoOp
        );
        assert_eq!(engine.observe_lock(locked_at), InactivityDecision::NoOp);
        assert_eq!(
            engine.time_until_action(activity_at),
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
