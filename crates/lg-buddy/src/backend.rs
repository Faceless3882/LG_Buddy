use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, ConfigPathError, ScreenBackend};
use crate::session_bus::new_session_bus_client;
use crate::sources::desktop::gnome::{
    GNOME_IDLE_MONITOR_NAME, GNOME_REQUIRED_SERVICES_REASON, GNOME_SCREEN_SAVER_NAME,
    GNOME_SHELL_NAME,
};
use crate::sources::desktop::wayland::probe_wayland_capabilities;

const GNOME_SHELL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSelectionError {
    InvalidOverride(String),
}

impl fmt::Display for BackendSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOverride(value) => write!(
                f,
                "invalid LG_BUDDY_SCREEN_BACKEND value `{value}`; expected auto, gnome, wayland, or swayidle"
            ),
        }
    }
}

impl Error for BackendSelectionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDetectionError {
    NoSupportedBackend,
    UnavailableBackend {
        backend: ScreenBackend,
        reason: String,
    },
    MissingRequiredCommand {
        backend: ScreenBackend,
        command: &'static str,
    },
}

impl fmt::Display for BackendDetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSupportedBackend => {
                write!(
                    f,
                    "no supported backend detected; install swayidle or run under a compatible GNOME session"
                )
            }
            Self::UnavailableBackend { backend, reason } => {
                write!(f, "backend `{}` is unavailable: {reason}", backend.as_str())
            }
            Self::MissingRequiredCommand { backend, command } => write!(
                f,
                "backend `{}` requires `{command}` to be installed",
                backend.as_str()
            ),
        }
    }
}

impl Error for BackendDetectionError {}

pub trait BackendProbe {
    fn has_command(&self, command: &str) -> bool;
    fn gnome_shell_available(&self) -> bool;
    fn gnome_screen_saver_available(&self) -> bool;
    fn gnome_idle_monitor_available(&self) -> bool;
    fn wayland_capabilities(&self) -> Result<(), String> {
        Err("native Wayland capability probing is unavailable".to_string())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBackendProbe;

impl BackendProbe for SystemBackendProbe {
    fn has_command(&self, command: &str) -> bool {
        command_in_path(command)
    }

    fn gnome_shell_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        if bus.name_has_owner(GNOME_SHELL_NAME).unwrap_or(false) {
            return true;
        }

        bus.wait_for_name(GNOME_SHELL_NAME, GNOME_SHELL_WAIT_TIMEOUT)
            .is_ok()
    }

    fn gnome_screen_saver_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false)
    }

    fn gnome_idle_monitor_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false)
    }

    fn wayland_capabilities(&self) -> Result<(), String> {
        probe_wayland_capabilities()
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

pub fn configured_backend_from_env_or_config() -> Result<ScreenBackend, BackendSelectionError> {
    let override_value = env::var("LG_BUDDY_SCREEN_BACKEND").ok();
    let config_backend = match resolve_config_path_from_env() {
        Ok(path) => load_config(&path).ok().map(|config| config.screen_backend),
        Err(ConfigPathError::NotConfigured) => None,
    };

    configured_backend_from_sources(override_value.as_deref(), config_backend)
}

pub fn configured_backend_from_sources(
    override_value: Option<&str>,
    config_backend: Option<ScreenBackend>,
) -> Result<ScreenBackend, BackendSelectionError> {
    if let Some(value) = override_value {
        return value
            .parse::<ScreenBackend>()
            .map_err(|_| BackendSelectionError::InvalidOverride(value.to_string()));
    }

    Ok(config_backend.unwrap_or(ScreenBackend::Auto))
}

pub fn detect_backend_from_system(
    configured: ScreenBackend,
) -> Result<ScreenBackend, BackendDetectionError> {
    detect_backend_with_probe(&SystemBackendProbe, configured)
}

pub fn detect_backend_with_probe(
    probe: &impl BackendProbe,
    configured: ScreenBackend,
) -> Result<ScreenBackend, BackendDetectionError> {
    match configured {
        ScreenBackend::Auto => {
            if probe.gnome_shell_available() {
                if probe.gnome_screen_saver_available() && probe.gnome_idle_monitor_available() {
                    return Ok(ScreenBackend::Gnome);
                }

                if probe.has_command("swayidle") {
                    return Ok(ScreenBackend::Swayidle);
                }

                return Err(BackendDetectionError::UnavailableBackend {
                    backend: ScreenBackend::Gnome,
                    reason: GNOME_REQUIRED_SERVICES_REASON.to_string(),
                });
            }

            if probe.has_command("swayidle") {
                return Ok(ScreenBackend::Swayidle);
            }

            Err(BackendDetectionError::NoSupportedBackend)
        }
        ScreenBackend::Gnome => {
            if probe.gnome_shell_available()
                && probe.gnome_screen_saver_available()
                && probe.gnome_idle_monitor_available()
            {
                Ok(ScreenBackend::Gnome)
            } else {
                Err(BackendDetectionError::UnavailableBackend {
                    backend: ScreenBackend::Gnome,
                    reason: GNOME_REQUIRED_SERVICES_REASON.to_string(),
                })
            }
        }
        ScreenBackend::Wayland => probe
            .wayland_capabilities()
            .map(|()| ScreenBackend::Wayland)
            .map_err(|reason| BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Wayland,
                reason,
            }),
        ScreenBackend::Swayidle => {
            if probe.has_command("swayidle") {
                Ok(ScreenBackend::Swayidle)
            } else {
                Err(BackendDetectionError::MissingRequiredCommand {
                    backend: ScreenBackend::Swayidle,
                    command: "swayidle",
                })
            }
        }
    }
}

fn command_in_path(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    let Some(path) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

#[cfg(test)]
mod tests {
    use super::{
        configured_backend_from_sources, detect_backend_with_probe, BackendDetectionError,
        BackendProbe, BackendSelectionError,
    };
    use crate::config::ScreenBackend;

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        gnome_shell_available: bool,
        gnome_screen_saver_available: bool,
        gnome_idle_monitor_available: bool,
        has_swayidle: bool,
    }

    impl BackendProbe for FakeProbe {
        fn has_command(&self, command: &str) -> bool {
            match command {
                "swayidle" => self.has_swayidle,
                _ => false,
            }
        }

        fn gnome_shell_available(&self) -> bool {
            self.gnome_shell_available
        }

        fn gnome_screen_saver_available(&self) -> bool {
            self.gnome_screen_saver_available
        }

        fn gnome_idle_monitor_available(&self) -> bool {
            self.gnome_idle_monitor_available
        }
    }

    struct WaylandProbe(Result<(), &'static str>);

    impl BackendProbe for WaylandProbe {
        fn has_command(&self, _command: &str) -> bool {
            false
        }

        fn gnome_shell_available(&self) -> bool {
            false
        }

        fn gnome_screen_saver_available(&self) -> bool {
            false
        }

        fn gnome_idle_monitor_available(&self) -> bool {
            false
        }

        fn wayland_capabilities(&self) -> Result<(), String> {
            self.0.map_err(str::to_string)
        }
    }

    #[test]
    fn env_override_wins_over_config_backend() {
        let backend = configured_backend_from_sources(Some("swayidle"), Some(ScreenBackend::Gnome))
            .expect("parse override backend");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn config_backend_is_used_when_override_is_missing() {
        let backend = configured_backend_from_sources(None, Some(ScreenBackend::Gnome))
            .expect("use config backend");

        assert_eq!(backend, ScreenBackend::Gnome);
    }

    #[test]
    fn auto_is_used_when_no_override_or_config_is_available() {
        let backend =
            configured_backend_from_sources(None, None).expect("fallback to auto backend");

        assert_eq!(backend, ScreenBackend::Auto);
    }

    #[test]
    fn wayland_override_is_accepted() {
        let backend = configured_backend_from_sources(Some("wayland"), None)
            .expect("parse native Wayland backend");

        assert_eq!(backend, ScreenBackend::Wayland);
    }

    #[test]
    fn invalid_override_is_rejected() {
        let err = configured_backend_from_sources(Some("kde"), None)
            .expect_err("invalid override should fail");

        assert_eq!(
            err,
            BackendSelectionError::InvalidOverride("kde".to_string())
        );
    }

    #[test]
    fn auto_prefers_gnome_when_available() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            has_swayidle: true,
        };

        let backend =
            detect_backend_with_probe(&probe, ScreenBackend::Auto).expect("detect gnome backend");

        assert_eq!(backend, ScreenBackend::Gnome);
    }

    #[test]
    fn auto_falls_back_to_swayidle() {
        let probe = FakeProbe {
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let backend = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("detect swayidle backend");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn auto_errors_when_no_supported_backend_is_available() {
        let probe = FakeProbe {
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("missing backend should fail");

        assert_eq!(err, BackendDetectionError::NoSupportedBackend);
    }

    #[test]
    fn forced_gnome_requires_full_service_surface() {
        let probe = FakeProbe {
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without a full GNOME session should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn auto_reports_gnome_unavailable_when_idle_monitor_is_missing_and_no_fallback_exists() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("unsupported gnome surface should fail explicitly");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn auto_falls_back_to_swayidle_when_gnome_idle_monitor_is_missing() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let backend = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("fallback to swayidle when GNOME is incomplete");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn forced_gnome_requires_idle_monitor() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without idle monitor should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn forced_swayidle_requires_command() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Swayidle)
            .expect_err("forced swayidle without command should fail");

        assert_eq!(
            err,
            BackendDetectionError::MissingRequiredCommand {
                backend: ScreenBackend::Swayidle,
                command: "swayidle",
            }
        );
    }

    #[test]
    fn forced_wayland_requires_the_native_protocol_surface() {
        let err = detect_backend_with_probe(
            &WaylandProbe(Err(
                "ext_idle_notifier_v1 version 1 is unsupported; version 2 or newer is required",
            )),
            ScreenBackend::Wayland,
        )
        .expect_err("forced Wayland without protocol v2 should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Wayland,
                reason:
                    "ext_idle_notifier_v1 version 1 is unsupported; version 2 or newer is required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn forced_wayland_is_selected_when_the_native_protocol_surface_is_available() {
        let backend = detect_backend_with_probe(&WaylandProbe(Ok(())), ScreenBackend::Wayland)
            .expect("forced Wayland should be available");

        assert_eq!(backend, ScreenBackend::Wayland);
    }
}
