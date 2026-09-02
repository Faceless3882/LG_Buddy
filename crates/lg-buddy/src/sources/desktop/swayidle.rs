use std::process::Command;

use crate::backend::{BackendProbe, SystemBackendProbe};
use crate::config::ScreenBackend;
use crate::session::{
    IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
    SessionEvent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwayidleHook {
    Timeout,
    Resume,
    BeforeSleep,
    AfterResume,
    Lock,
    Unlock,
}

impl SwayidleHook {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Resume => "resume",
            Self::BeforeSleep => "before-sleep",
            Self::AfterResume => "after-resume",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
        }
    }

    pub fn session_event(self) -> SessionEvent {
        match self {
            Self::Timeout => SessionEvent::Idle,
            Self::Resume => SessionEvent::Active,
            Self::BeforeSleep => SessionEvent::BeforeSleep,
            Self::AfterResume => SessionEvent::AfterResume,
            Self::Lock => SessionEvent::Lock,
            Self::Unlock => SessionEvent::Unlock,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwayidleBackendStatus {
    pub command_available: bool,
    pub systemd_hooks_available: bool,
}

impl SwayidleBackendStatus {
    pub fn can_start(&self) -> bool {
        self.command_available
    }

    pub fn supports_hook(&self, hook: SwayidleHook) -> bool {
        match hook {
            SwayidleHook::Timeout | SwayidleHook::Resume => self.command_available,
            SwayidleHook::BeforeSleep
            | SwayidleHook::AfterResume
            | SwayidleHook::Lock
            | SwayidleHook::Unlock => self.command_available && self.systemd_hooks_available,
        }
    }
}

pub trait SwayidleProbe {
    fn swayidle_available(&self) -> bool;
    fn systemd_hooks_available(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSwayidleProbe;

impl SwayidleProbe for SystemSwayidleProbe {
    fn swayidle_available(&self) -> bool {
        let probe = SystemBackendProbe::default();
        probe.has_command("swayidle")
    }

    fn systemd_hooks_available(&self) -> bool {
        let output = Command::new("swayidle").arg("-h").output();
        let Ok(output) = output else {
            return false;
        };

        let rendered = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        swayidle_help_output_supports_systemd_hooks(&rendered)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SwayidleBackend<P = SystemSwayidleProbe> {
    probe: P,
}

impl Default for SwayidleBackend<SystemSwayidleProbe> {
    fn default() -> Self {
        Self::new(SystemSwayidleProbe)
    }
}

impl<P> SwayidleBackend<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl<P: SwayidleProbe> SwayidleBackend<P> {
    pub fn status(&self) -> SwayidleBackendStatus {
        SwayidleBackendStatus {
            command_available: self.probe.swayidle_available(),
            systemd_hooks_available: self.probe.systemd_hooks_available(),
        }
    }
}

impl<P: SwayidleProbe> SessionBackend for SwayidleBackend<P> {
    fn backend(&self) -> ScreenBackend {
        ScreenBackend::Swayidle
    }

    fn capabilities(&self) -> Result<SessionBackendCapabilities, SessionBackendError> {
        let status = self.status();
        if !status.can_start() {
            return Err(SessionBackendError::Unavailable {
                backend: ScreenBackend::Swayidle,
                reason: "swayidle is required",
            });
        }

        Ok(SessionBackendCapabilities {
            idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
            wake_requested: false,
            before_sleep: status.systemd_hooks_available,
            after_resume: status.systemd_hooks_available,
            lock_unlock: status.systemd_hooks_available,
            early_user_activity: false,
        })
    }
}

fn swayidle_help_output_supports_systemd_hooks(output: &str) -> bool {
    [
        SwayidleHook::BeforeSleep,
        SwayidleHook::AfterResume,
        SwayidleHook::Lock,
        SwayidleHook::Unlock,
    ]
    .iter()
    .all(|hook| output.contains(hook.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{
        swayidle_help_output_supports_systemd_hooks, SwayidleBackend, SwayidleBackendStatus,
        SwayidleHook, SwayidleProbe,
    };
    use crate::config::ScreenBackend;
    use crate::session::{
        IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
        SessionEvent,
    };

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        swayidle_available: bool,
        systemd_hooks_available: bool,
    }

    impl SwayidleProbe for FakeProbe {
        fn swayidle_available(&self) -> bool {
            self.swayidle_available
        }

        fn systemd_hooks_available(&self) -> bool {
            self.systemd_hooks_available
        }
    }

    #[test]
    fn timeout_hook_maps_to_idle_event() {
        assert_eq!(SwayidleHook::Timeout.session_event(), SessionEvent::Idle);
    }

    #[test]
    fn resume_hook_maps_to_active_event() {
        assert_eq!(SwayidleHook::Resume.session_event(), SessionEvent::Active);
    }

    #[test]
    fn systemd_hooks_map_to_session_events() {
        assert_eq!(
            SwayidleHook::BeforeSleep.session_event(),
            SessionEvent::BeforeSleep
        );
        assert_eq!(
            SwayidleHook::AfterResume.session_event(),
            SessionEvent::AfterResume
        );
        assert_eq!(SwayidleHook::Lock.session_event(), SessionEvent::Lock);
        assert_eq!(SwayidleHook::Unlock.session_event(), SessionEvent::Unlock);
    }

    #[test]
    fn help_output_detects_systemd_hooks() {
        let help = "\
timeout <timeout> <timeout command> [resume <resume command>]\n\
before-sleep <command>\n\
after-resume <command>\n\
lock <command>\n\
unlock <command>\n";

        assert!(swayidle_help_output_supports_systemd_hooks(help));
    }

    #[test]
    fn help_output_rejects_partial_systemd_hook_surface() {
        let help = "\
timeout <timeout> <timeout command> [resume <resume command>]\n\
before-sleep <command>\n";

        assert!(!swayidle_help_output_supports_systemd_hooks(help));
    }

    #[test]
    fn swayidle_backend_reports_minimal_capabilities_without_systemd_hooks() {
        let backend = SwayidleBackend::new(FakeProbe {
            swayidle_available: true,
            systemd_hooks_available: false,
        });

        assert_eq!(backend.backend(), ScreenBackend::Swayidle);
        assert_eq!(
            backend.capabilities().expect("backend should be available"),
            SessionBackendCapabilities {
                idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
                wake_requested: false,
                before_sleep: false,
                after_resume: false,
                lock_unlock: false,
                early_user_activity: false,
            }
        );
    }

    #[test]
    fn swayidle_backend_reports_extended_capabilities_with_systemd_hooks() {
        let backend = SwayidleBackend::new(FakeProbe {
            swayidle_available: true,
            systemd_hooks_available: true,
        });

        assert_eq!(
            backend.capabilities().expect("backend should be available"),
            SessionBackendCapabilities {
                idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
                wake_requested: false,
                before_sleep: true,
                after_resume: true,
                lock_unlock: true,
                early_user_activity: false,
            }
        );
    }

    #[test]
    fn swayidle_backend_requires_command() {
        let backend = SwayidleBackend::new(FakeProbe {
            swayidle_available: false,
            systemd_hooks_available: true,
        });

        assert_eq!(
            backend.capabilities(),
            Err(SessionBackendError::Unavailable {
                backend: ScreenBackend::Swayidle,
                reason: "swayidle is required",
            })
        );
    }

    #[test]
    fn status_supports_timeout_and_resume_without_systemd_hooks() {
        let status = SwayidleBackendStatus {
            command_available: true,
            systemd_hooks_available: false,
        };

        assert!(status.supports_hook(SwayidleHook::Timeout));
        assert!(status.supports_hook(SwayidleHook::Resume));
        assert!(!status.supports_hook(SwayidleHook::BeforeSleep));
        assert!(!status.supports_hook(SwayidleHook::Unlock));
    }

    #[test]
    fn status_supports_all_hooks_with_systemd_support() {
        let status = SwayidleBackendStatus {
            command_available: true,
            systemd_hooks_available: true,
        };

        assert!(status.supports_hook(SwayidleHook::Timeout));
        assert!(status.supports_hook(SwayidleHook::Resume));
        assert!(status.supports_hook(SwayidleHook::BeforeSleep));
        assert!(status.supports_hook(SwayidleHook::AfterResume));
        assert!(status.supports_hook(SwayidleHook::Lock));
        assert!(status.supports_hook(SwayidleHook::Unlock));
    }
}
