use std::env;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

use super::SettingsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyOutcome {
    Restarted { service: &'static str },
    Enabled { unit: &'static str },
    EnabledStarted { unit: &'static str },
    DisabledStopped { unit: &'static str },
    NotInstalled { service: &'static str },
    InactiveDisabled { service: &'static str },
    Skipped { reason: String },
    NoActionRequired,
}

impl fmt::Display for SettingsApplyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Restarted { service } => write!(f, "restarted {service}"),
            Self::Enabled { unit } => write!(f, "enabled {unit}"),
            Self::EnabledStarted { unit } => write!(f, "enabled and started {unit}"),
            Self::DisabledStopped { unit } => write!(f, "disabled and stopped {unit}"),
            Self::NotInstalled { service } => {
                write!(
                    f,
                    "{service} is not installed; change applies when it is installed"
                )
            }
            Self::InactiveDisabled { service } => write!(
                f,
                "{service} is inactive and disabled; change applies when it is started"
            ),
            Self::Skipped { reason } => write!(f, "{reason}"),
            Self::NoActionRequired => write!(f, "no runtime apply action required"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserServiceState {
    Missing,
    InactiveDisabled,
    ActiveOrEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserUnitEnableOutcome {
    Enabled,
    EnabledStarted,
}

pub trait ServiceController {
    fn systemd_actions_disabled(&self) -> bool {
        false
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError>;

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError>;

    fn enable_start_user_unit(&self, unit: &str) -> Result<UserUnitEnableOutcome, SettingsError>;

    fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError>;
}

#[derive(Debug, Clone)]
pub struct SystemdUserServiceController {
    command_path: PathBuf,
    skip_systemd_actions: bool,
}

impl Default for SystemdUserServiceController {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemdUserServiceController {
    pub fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
            skip_systemd_actions: env_truthy("LG_BUDDY_SKIP_SYSTEMD_ACTIONS"),
        }
    }

    fn user_systemctl_status(&self, args: &[&str]) -> io::Result<bool> {
        ProcessCommand::new(&self.command_path)
            .arg("--user")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
    }

    fn run_user_systemctl(&self, args: &[&str]) -> Result<(), SettingsError> {
        let output = ProcessCommand::new(&self.command_path)
            .arg("--user")
            .args(args)
            .output()
            .map_err(|err| SettingsError::Apply {
                message: format!("could not run systemctl: {err}"),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(SettingsError::Apply {
                message: format_command_failure(
                    output.status.code(),
                    &output.stdout,
                    &output.stderr,
                ),
            })
        }
    }
}

impl ServiceController for SystemdUserServiceController {
    fn systemd_actions_disabled(&self) -> bool {
        self.skip_systemd_actions
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError> {
        if !self
            .user_systemctl_status(&["cat", service])
            .unwrap_or(false)
        {
            return Ok(UserServiceState::Missing);
        }

        let active = self
            .user_systemctl_status(&["is-active", "--quiet", service])
            .unwrap_or(false);
        let enabled = self
            .user_systemctl_status(&["is-enabled", "--quiet", service])
            .unwrap_or(false);

        if active || enabled {
            Ok(UserServiceState::ActiveOrEnabled)
        } else {
            Ok(UserServiceState::InactiveDisabled)
        }
    }

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["restart", service])
    }

    fn enable_start_user_unit(&self, unit: &str) -> Result<UserUnitEnableOutcome, SettingsError> {
        self.run_user_systemctl(&["enable", unit])?;
        if self
            .user_systemctl_status(&["is-active", "--quiet", "graphical-session.target"])
            .unwrap_or(false)
        {
            self.run_user_systemctl(&["start", unit])?;
            Ok(UserUnitEnableOutcome::EnabledStarted)
        } else {
            Ok(UserUnitEnableOutcome::Enabled)
        }
    }

    fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["disable", "--now", unit])
    }
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "Yes"
            )
        })
        .unwrap_or(false)
}

fn format_command_failure(status_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let status = status_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => format!("systemctl exited with status {status}"),
        (false, true) => format!("systemctl exited with status {status}: {stdout}"),
        (true, false) => format!("systemctl exited with status {status}: {stderr}"),
        (false, false) => format!("systemctl exited with status {status}: {stderr}; {stdout}"),
    }
}
