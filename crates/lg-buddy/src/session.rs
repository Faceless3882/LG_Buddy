pub mod gamepad;
pub mod inactivity;
pub mod runner;

use std::time::Instant;

use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Idle,
    Active,
    WakeRequested,
    BeforeSleep,
    AfterResume,
    Lock,
    Unlock,
    UserActivity,
}

impl SessionEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::WakeRequested => "wake-requested",
            Self::BeforeSleep => "before-sleep",
            Self::AfterResume => "after-resume",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::UserActivity => "user-activity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionObservation {
    Event {
        event: SessionEvent,
        source: EventSource,
        observed_at: Instant,
    },
    Inactivity {
        observation: InactivityObservation,
        source: EventSource,
        observed_at: Instant,
    },
}
