mod dev;

pub mod auth;
pub mod backend;
pub mod commands;
pub mod config;
pub mod events;
pub mod lifecycle;
pub mod notifications;
pub mod platform_access_token;
pub mod policy;
pub mod runtime_phase;
pub mod screen;
pub mod session;
pub mod session_bus;
pub mod session_notifications;
pub mod settings;
pub mod sources;
pub mod state;
pub mod tv;
pub mod updates;
pub mod version;
pub mod web_os;
pub mod wol;

pub use dev::{DevCommand, DevError, DevParseError, WebOsControlProbeCommand};
pub use sources::desktop::{gnome, swayidle};
pub use sources::linux::{logind, network_manager};

use crate::auth::AuthContextError;
use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{
    run_brightness, run_nm_pre_down, run_screen_off, run_screen_on, run_shutdown, run_sleep,
    run_sleep_pre,
};
use crate::config::{ConfigError, ConfigPathError};
use crate::dev::run_dev_command;
use crate::notifications::NotificationError;
use crate::session::runner::{run_lifecycle_monitor, run_monitor};
use crate::settings::{run_settings_command, SettingsCommand, SettingsError, SettingsParseError};
use crate::state::StateDirError;
use crate::tv::{OledBrightness, OledBrightnessParseError};
use crate::updates::{run_updates_command, UpdatesCommand, UpdatesError, UpdatesParseError};
use std::fmt;
use std::io::{self, Write};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Startup(StartupMode),
    Shutdown,
    SleepPre,
    Sleep,
    NetworkManagerPreDown,
    Brightness(BrightnessCommand),
    ScreenOff,
    ScreenOn,
    Monitor,
    Lifecycle,
    DetectBackend,
    Dev(DevCommand),
    Settings(SettingsCommand),
    Updates(UpdatesCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessCommand {
    Prompt,
    Get,
    Set(OledBrightness),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Auto,
    Boot,
    Wake,
}

impl StartupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Boot => "boot",
            Self::Wake => "wake",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "boot" => Some(Self::Boot),
            "wake" => Some(Self::Wake),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Help,
    Version,
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnknownCommand(String),
    UnknownStartupMode(String),
    UnknownBrightnessCommand(String),
    MissingBrightnessValue,
    InvalidBrightnessValue(OledBrightnessParseError),
    Dev(DevParseError),
    Settings(SettingsParseError),
    Updates(UpdatesParseError),
    UnexpectedArguments {
        command: Command,
        arguments: Vec<String>,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(command) => {
                write!(f, "unknown command `{command}`")
            }
            Self::UnknownStartupMode(mode) => {
                write!(f, "unknown startup mode `{mode}`")
            }
            Self::UnknownBrightnessCommand(command) => {
                write!(f, "unknown brightness command `{command}`")
            }
            Self::MissingBrightnessValue => {
                write!(f, "missing brightness value for `brightness set`")
            }
            Self::InvalidBrightnessValue(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::UnexpectedArguments { command, arguments } => {
                write!(
                    f,
                    "unexpected arguments for `{}`: {}",
                    command.as_str(),
                    arguments.join(" ")
                )
            }
        }
    }
}

#[derive(Debug)]
pub enum RunError {
    Io(io::Error),
    Policy(String),
    AuthContext(AuthContextError),
    ConfigPath(ConfigPathError),
    Config(ConfigError),
    StateDir(StateDirError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
    Dev(DevError),
    Settings(SettingsError),
    Updates(UpdatesError),
    NotificationAfterPrimary {
        primary: Box<RunError>,
        notification: NotificationError,
    },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Policy(err) => write!(f, "{err}"),
            Self::AuthContext(err) => write!(f, "{err}"),
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::StateDir(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::NotificationAfterPrimary {
                primary,
                notification,
            } => write!(
                f,
                "{primary}; additionally, desktop notification failed: {notification}"
            ),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Policy(_) => None,
            Self::AuthContext(err) => Some(err),
            Self::ConfigPath(err) => Some(err),
            Self::Config(err) => Some(err),
            Self::StateDir(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
            Self::Dev(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::Updates(err) => Some(err),
            Self::NotificationAfterPrimary { primary, .. } => Some(primary.as_ref()),
        }
    }
}

impl From<io::Error> for RunError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl Command {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup(_) => "startup",
            Self::Shutdown => "shutdown",
            Self::SleepPre => "sleep-pre",
            Self::Sleep => "sleep",
            Self::NetworkManagerPreDown => "nm-pre-down",
            Self::Brightness(_) => "brightness",
            Self::ScreenOff => "screen-off",
            Self::ScreenOn => "screen-on",
            Self::Monitor => "monitor",
            Self::Lifecycle => "lifecycle",
            Self::DetectBackend => "detect-backend",
            Self::Dev(command) => command.as_str(),
            Self::Settings(_) => "settings",
            Self::Updates(_) => "updates",
        }
    }

    pub fn placeholder_message(&self) -> &'static str {
        match self {
            Self::Startup(_) => "TODO: implemented via command handler",
            Self::Shutdown => "TODO: implemented via command handler",
            Self::SleepPre => "TODO: implemented via command handler",
            Self::Sleep => "TODO: implemented via command handler",
            Self::NetworkManagerPreDown => "TODO: implemented via command handler",
            Self::Brightness(_) => "TODO: implemented via command handler",
            Self::ScreenOff => "TODO: implemented via command handler",
            Self::ScreenOn => "TODO: implemented via command handler",
            Self::Monitor => "TODO: implemented via command handler",
            Self::Lifecycle => "TODO: implemented via command handler",
            Self::DetectBackend => "TODO: implement detect-backend command",
            Self::Dev(_) => "TODO: implemented via temporary dev command handler",
            Self::Settings(_) => "TODO: implemented via command handler",
            Self::Updates(_) => "TODO: implemented via command handler",
        }
    }
}

pub fn usage(program: &str) -> String {
    format!(
        "\
LG Buddy Rust runtime

Usage:
  {program} <command>
  {program} --help
  {program} --version, -V

Commands:
  startup [mode]  Start or restore the TV output
  shutdown        Power off the TV when LG Buddy owns the active input
  sleep-pre       Handle the pre-sleep TV power-off hook
  sleep           Handle the NetworkManager pre-down sleep hook
  nm-pre-down     Handle NetworkManager pre-down system sleep gate
  brightness      Open the TV brightness control dialog
  brightness get  Print the current TV OLED brightness
  brightness set <0-100>
                  Set the TV OLED brightness
  screen-off      Blank the configured TV output if active
  screen-on       Restore the TV output after an LG Buddy screen-off
  monitor         Run the user-session monitor loop
  lifecycle       Run the system lifecycle monitor loop
  detect-backend  Detect the active screen backend
  settings        Inspect and edit structured LG Buddy settings
  updates         Check GitHub releases on demand or from the user timer

Startup modes:
  auto            Restore on wake when LG Buddy owns the system marker, otherwise boot
  boot            Always treat startup as a cold boot
  wake            Only restore when LG Buddy owns the system marker

Settings:
  settings list
  settings describe [key]
  settings get <key>
  settings set <key> <value>
  settings unset <key>

Updates:
  updates check [--channel stable|prerelease] [--notify]
  updates background-check
"
    )
}

pub fn parse_args<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(ParseOutcome::Help);
    };

    let first = first.as_ref();
    if matches!(first, "-h" | "--help" | "help") {
        return Ok(ParseOutcome::Help);
    }
    if matches!(first, "-V" | "--version") {
        return Ok(ParseOutcome::Version);
    }

    let command = match first {
        "startup" => {
            let startup_mode = match args.next() {
                Some(mode) => {
                    let mode = mode.as_ref();
                    StartupMode::parse(mode)
                        .ok_or_else(|| ParseError::UnknownStartupMode(mode.to_string()))?
                }
                None => StartupMode::Auto,
            };

            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            if !extra_args.is_empty() {
                return Err(ParseError::UnexpectedArguments {
                    command: Command::Startup(startup_mode),
                    arguments: extra_args,
                });
            }

            return Ok(ParseOutcome::Command(Command::Startup(startup_mode)));
        }
        "settings" => {
            return SettingsCommand::parse(args)
                .map(|command| ParseOutcome::Command(Command::Settings(command)))
                .map_err(ParseError::Settings);
        }
        "updates" => {
            return UpdatesCommand::parse(args)
                .map(|command| ParseOutcome::Command(Command::Updates(command)))
                .map_err(ParseError::Updates);
        }
        "dev" => {
            return DevCommand::parse(args)
                .map(|command| ParseOutcome::Command(Command::Dev(command)))
                .map_err(ParseError::Dev);
        }
        "brightness" => return parse_brightness_command(args),
        "shutdown" => Command::Shutdown,
        "sleep-pre" => Command::SleepPre,
        "sleep" => Command::Sleep,
        "nm-pre-down" => Command::NetworkManagerPreDown,
        "screen-off" => Command::ScreenOff,
        "screen-on" => Command::ScreenOn,
        "monitor" => Command::Monitor,
        "lifecycle" => Command::Lifecycle,
        "detect-backend" => Command::DetectBackend,
        other => return Err(ParseError::UnknownCommand(other.to_string())),
    };

    let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
    if !extra_args.is_empty() {
        return Err(ParseError::UnexpectedArguments {
            command,
            arguments: extra_args,
        });
    }

    Ok(ParseOutcome::Command(command))
}

pub fn run_command<W: Write>(command: Command, writer: &mut W) -> Result<(), RunError> {
    match command {
        Command::Startup(mode) => crate::commands::run_startup(writer, mode),
        Command::Shutdown => run_shutdown(writer),
        Command::SleepPre => run_sleep_pre(writer),
        Command::Sleep => run_sleep(writer),
        Command::NetworkManagerPreDown => run_nm_pre_down(writer),
        Command::Brightness(command) => run_brightness(writer, command),
        Command::DetectBackend => run_detect_backend(writer),
        Command::ScreenOff => run_screen_off(writer),
        Command::ScreenOn => run_screen_on(writer),
        Command::Monitor => run_monitor(writer),
        Command::Lifecycle => run_lifecycle_monitor(writer),
        Command::Dev(command) => run_dev_command(command, writer).map_err(RunError::Dev),
        Command::Settings(command) => {
            run_settings_command(command, writer).map_err(RunError::Settings)
        }
        Command::Updates(command) => {
            run_updates_command(command, writer).map_err(RunError::Updates)
        }
    }
}

fn parse_brightness_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Brightness(
            BrightnessCommand::Prompt,
        )));
    };

    match subcommand.as_ref() {
        "get" => {
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(
                    BrightnessCommand::Get,
                )))
            } else {
                Err(ParseError::UnexpectedArguments {
                    command: Command::Brightness(BrightnessCommand::Get),
                    arguments: extra_args,
                })
            }
        }
        "set" => {
            let value = args.next().ok_or(ParseError::MissingBrightnessValue)?;
            let brightness = OledBrightness::parse(value.as_ref())
                .map_err(ParseError::InvalidBrightnessValue)?;
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            let command = BrightnessCommand::Set(brightness);
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(command)))
            } else {
                Err(ParseError::UnexpectedArguments {
                    command: Command::Brightness(command),
                    arguments: extra_args,
                })
            }
        }
        other => Err(ParseError::UnknownBrightnessCommand(other.to_string())),
    }
}

fn run_detect_backend<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let configured = configured_backend_from_env_or_config().map_err(RunError::BackendSelection)?;
    let backend = detect_backend_from_system(configured).map_err(RunError::BackendDetection)?;

    writeln!(writer, "{}", backend.as_str())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_args, usage, BrightnessCommand, Command, DevCommand, DevParseError, ParseError,
        ParseOutcome, StartupMode, WebOsControlProbeCommand,
    };
    use crate::settings::{SettingsCommand, SettingsParseError};
    use crate::tv::OledBrightness;
    use crate::updates::{UpdateChannel, UpdatesCommand, UpdatesParseError};
    use crate::{notifications::NotificationError, RunError};
    use std::error::Error;
    use std::io;

    #[test]
    fn no_args_prints_help() {
        assert_eq!(parse_args(Vec::<String>::new()), Ok(ParseOutcome::Help));
    }

    #[test]
    fn explicit_help_prints_help() {
        assert_eq!(parse_args(["--help"]), Ok(ParseOutcome::Help));
        assert_eq!(parse_args(["-h"]), Ok(ParseOutcome::Help));
        assert_eq!(parse_args(["help"]), Ok(ParseOutcome::Help));
    }

    #[test]
    fn explicit_version_prints_version() {
        assert_eq!(parse_args(["--version"]), Ok(ParseOutcome::Version));
        assert_eq!(parse_args(["-V"]), Ok(ParseOutcome::Version));
    }

    #[test]
    fn supported_commands_parse() {
        assert_eq!(
            parse_args(["startup"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Auto)))
        );
        assert_eq!(
            parse_args(["startup", "boot"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Boot)))
        );
        assert_eq!(
            parse_args(["startup", "wake"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Wake)))
        );
        assert_eq!(
            parse_args(["shutdown"]),
            Ok(ParseOutcome::Command(Command::Shutdown))
        );
        assert_eq!(
            parse_args(["sleep-pre"]),
            Ok(ParseOutcome::Command(Command::SleepPre))
        );
        assert_eq!(
            parse_args(["sleep"]),
            Ok(ParseOutcome::Command(Command::Sleep))
        );
        assert_eq!(
            parse_args(["nm-pre-down"]),
            Ok(ParseOutcome::Command(Command::NetworkManagerPreDown))
        );
        assert_eq!(
            parse_args(["brightness"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Prompt
            )))
        );
        assert_eq!(
            parse_args(["brightness", "get"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Get
            )))
        );
        assert_eq!(
            parse_args(["brightness", "set", "65"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Set(brightness(65))
            )))
        );
        assert_eq!(
            parse_args(["screen-off"]),
            Ok(ParseOutcome::Command(Command::ScreenOff))
        );
        assert_eq!(
            parse_args(["screen-on"]),
            Ok(ParseOutcome::Command(Command::ScreenOn))
        );
        assert_eq!(
            parse_args(["monitor"]),
            Ok(ParseOutcome::Command(Command::Monitor))
        );
        assert_eq!(
            parse_args(["lifecycle"]),
            Ok(ParseOutcome::Command(Command::Lifecycle))
        );
        assert_eq!(
            parse_args(["detect-backend"]),
            Ok(ParseOutcome::Command(Command::DetectBackend))
        );
        assert_eq!(
            parse_args(["dev", "webos-auth-probe"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsAuthProbe
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-read-probe"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsReadProbe
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "set-input"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput)
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "screen-off"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::ScreenOff)
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "screen-on"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::ScreenOn)
            )))
        );
        assert_eq!(
            parse_args(["settings", "list"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::List
            )))
        );
        assert_eq!(
            parse_args(["settings", "describe"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Describe(None)
            )))
        );
        assert_eq!(
            parse_args(["settings", "describe", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Describe(Some("screen.backend".to_string()))
            )))
        );
        assert_eq!(
            parse_args(["settings", "get", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Get("screen.backend".to_string())
            )))
        );
        assert_eq!(
            parse_args(["settings", "set", "screen.backend", "gnome"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                }
            )))
        );
        assert_eq!(
            parse_args(["settings", "unset", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Unset("screen.backend".to_string())
            )))
        );
        assert_eq!(
            parse_args(["updates", "check"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: None,
                    notify: false,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "stable"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: Some(UpdateChannel::Stable),
                    notify: false,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "prerelease"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: Some(UpdateChannel::Prerelease),
                    notify: false,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: None,
                    notify: true,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify", "--channel", "stable"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: Some(UpdateChannel::Stable),
                    notify: true,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "prerelease", "--notify"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check {
                    channel: Some(UpdateChannel::Prerelease),
                    notify: true,
                }
            )))
        );
        assert_eq!(
            parse_args(["updates", "background-check"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::BackgroundCheck
            )))
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert_eq!(
            parse_args(["launch"]),
            Err(ParseError::UnknownCommand("launch".to_string()))
        );
    }

    #[test]
    fn extra_arguments_are_rejected() {
        assert_eq!(
            parse_args(["startup", "boot", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::Startup(StartupMode::Boot),
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn invalid_startup_mode_is_rejected() {
        assert_eq!(
            parse_args(["startup", "resume"]),
            Err(ParseError::UnknownStartupMode("resume".to_string()))
        );
    }

    #[test]
    fn invalid_brightness_command_is_rejected() {
        assert_eq!(
            parse_args(["brightness", "show"]),
            Err(ParseError::UnknownBrightnessCommand("show".to_string()))
        );
        assert_eq!(
            parse_args(["brightness", "set"]),
            Err(ParseError::MissingBrightnessValue)
        );
        assert!(matches!(
            parse_args(["brightness", "set", "101"]),
            Err(ParseError::InvalidBrightnessValue(_))
        ));
        assert!(matches!(
            parse_args(["brightness", "set", "abc"]),
            Err(ParseError::InvalidBrightnessValue(_))
        ));
        assert_eq!(
            parse_args(["brightness", "get", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::Brightness(BrightnessCommand::Get),
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn invalid_dev_command_is_rejected() {
        assert_eq!(
            parse_args(["dev"]),
            Err(ParseError::Dev(DevParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["dev", "other"]),
            Err(ParseError::Dev(DevParseError::UnknownSubcommand(
                "other".to_string()
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-auth-probe", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsAuthProbe,
                arguments: vec!["extra".to_string()],
            }))
        );
        assert_eq!(
            parse_args(["dev", "webos-read-probe", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsReadProbe,
                arguments: vec!["extra".to_string()],
            }))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe"]),
            Err(ParseError::Dev(DevParseError::MissingControlOperation))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "other"]),
            Err(ParseError::Dev(DevParseError::UnknownControlOperation(
                "other".to_string()
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "set-input", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput),
                arguments: vec!["extra".to_string()],
            }))
        );
    }

    #[test]
    fn invalid_settings_command_is_rejected() {
        assert_eq!(
            parse_args(["settings"]),
            Err(ParseError::Settings(SettingsParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["settings", "get"]),
            Err(ParseError::Settings(SettingsParseError::MissingKey {
                subcommand: "get",
            }))
        );
        assert_eq!(
            parse_args(["settings", "list", "extra"]),
            Err(ParseError::Settings(
                SettingsParseError::UnexpectedArguments {
                    subcommand: "list",
                    arguments: vec!["extra".to_string()],
                }
            ))
        );
    }

    #[test]
    fn invalid_updates_command_is_rejected() {
        assert_eq!(
            parse_args(["updates"]),
            Err(ParseError::Updates(UpdatesParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["updates", "latest"]),
            Err(ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                "latest".to_string()
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel"]),
            Err(ParseError::Updates(UpdatesParseError::MissingChannelValue))
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "nightly"]),
            Err(ParseError::Updates(UpdatesParseError::UnknownChannel(
                "nightly".to_string()
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "extra"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: vec!["extra".to_string()]
                }
            ))
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify", "--notify"]),
            Err(ParseError::Updates(UpdatesParseError::DuplicateNotify))
        );
        assert_eq!(
            parse_args(["updates", "background-check", "extra"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "background-check",
                    arguments: vec!["extra".to_string()]
                }
            ))
        );
    }

    #[test]
    fn usage_mentions_all_commands() {
        let help = usage("lg-buddy");

        for command in [
            "startup",
            "shutdown",
            "sleep-pre",
            "sleep",
            "nm-pre-down",
            "brightness",
            "screen-off",
            "screen-on",
            "monitor",
            "lifecycle",
            "detect-backend",
            "settings",
            "updates",
        ] {
            assert!(
                help.contains(command),
                "missing `{command}` from help output"
            );
        }
        assert!(!help.contains("webos-auth-probe"));
        assert!(!help.contains("webos-read-probe"));
    }

    #[test]
    fn usage_mentions_startup_modes() {
        let help = usage("lg-buddy");

        for mode in ["auto", "boot", "wake"] {
            assert!(help.contains(mode), "missing startup mode `{mode}`");
        }
    }

    #[test]
    fn usage_mentions_settings_commands_without_reserved_notice() {
        let help = usage("lg-buddy");

        for command in ["brightness get", "brightness set <0-100>"] {
            assert!(help.contains(command), "missing `{command}` from help");
        }

        for command in [
            "--version, -V",
            "settings list",
            "settings describe [key]",
            "settings get <key>",
            "settings set <key> <value>",
            "settings unset <key>",
            "updates check [--channel stable|prerelease] [--notify]",
            "updates background-check",
        ] {
            assert!(help.contains(command), "missing `{command}` from help");
        }
        assert!(!help.contains("Reserved for write support"));
    }

    #[test]
    fn notification_context_preserves_primary_run_error_source() {
        let err = RunError::NotificationAfterPrimary {
            primary: Box::new(RunError::Io(io::Error::other("disk unavailable"))),
            notification: NotificationError::Transport("bus unavailable".to_string()),
        };

        assert_eq!(
            err.to_string(),
            "disk unavailable; additionally, desktop notification failed: desktop notification service error: bus unavailable"
        );
        assert_eq!(
            err.source()
                .expect("primary source should be preserved")
                .to_string(),
            "disk unavailable"
        );
    }

    fn brightness(value: u8) -> OledBrightness {
        OledBrightness::new(value).expect("test brightness should be valid")
    }
}
