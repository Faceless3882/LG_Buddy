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
pub mod release_bundle;
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
pub mod upgrade_preflight;
pub mod version;
pub mod web_os;
pub mod wol;

pub use dev::{DevCommand, DevError, DevParseError, WebOsControlProbeCommand};
pub use sources::desktop::{gnome, swayidle, wayland};
pub use sources::linux::{logind, network_manager};

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{
    run_brightness, run_nm_pre_down, run_screen_off, run_screen_on, run_shutdown, run_sleep,
    run_sleep_pre, run_volume,
};
use crate::config::{ConfigError, ConfigPathError};
use crate::dev::run_dev_command;
use crate::notifications::NotificationError;
use crate::session::runner::{run_lifecycle_monitor, run_monitor};
use crate::settings::{run_settings_command, SettingsCommand, SettingsError, SettingsParseError};
use crate::state::StateDirError;
use crate::tv::{
    OledBrightness, OledBrightnessParseError, TvClientBuildError, VolumeLevel,
    VolumeLevelParseError,
};
use crate::updates::{run_updates_command, UpdatesCommand, UpdatesError, UpdatesParseError};
use crate::upgrade_preflight::CompatibilityReport;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Startup(StartupMode),
    Shutdown,
    Power(PowerCommand),
    SleepPre,
    Sleep,
    NetworkManagerPreDown,
    Brightness(BrightnessCommand),
    Volume(VolumeCommand),
    Screen(ScreenCommand),
    ScreenOff,
    ScreenOn,
    Monitor,
    Lifecycle,
    DetectBackend,
    Dev(DevCommand),
    Settings(SettingsCommand),
    Updates(UpdatesCommand),
    UpgradePreflight {
        candidate_root: PathBuf,
        repair_python: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessCommand {
    Prompt,
    Get,
    Set(OledBrightness),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCommand {
    Get,
    Set(VolumeLevel),
    Up,
    Down,
    Mute(MuteCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteCommand {
    Toggle,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCommand {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHelpTopic {
    Root,
    List,
    Describe,
    Get,
    Set,
    Unset,
}

impl SettingsHelpTopic {
    fn from_subcommand(subcommand: &str) -> Option<Self> {
        match subcommand {
            "list" => Some(Self::List),
            "describe" => Some(Self::Describe),
            "get" => Some(Self::Get),
            "set" => Some(Self::Set),
            "unset" => Some(Self::Unset),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatesHelpTopic {
    Root,
    Check,
}

impl UpdatesHelpTopic {
    fn from_subcommand(subcommand: &str) -> Option<Self> {
        match subcommand {
            "check" => Some(Self::Check),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTopic {
    Global,
    Brightness,
    Volume,
    Power,
    Screen,
    Settings(SettingsHelpTopic),
    Updates(UpdatesHelpTopic),
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
    Help(HelpTopic),
    Version,
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnknownCommand(String),
    MissingPowerCommand,
    UnknownPowerCommand(String),
    MissingScreenCommand,
    UnknownScreenCommand(String),
    UnknownStartupMode(String),
    UnknownBrightnessCommand(String),
    MissingBrightnessValue,
    InvalidBrightnessValue(OledBrightnessParseError),
    UnknownVolumeCommand(String),
    UnknownMuteCommand(String),
    InvalidVolumeValue(VolumeLevelParseError),
    Dev(DevParseError),
    Settings(SettingsParseError),
    Updates(UpdatesParseError),
    MissingUpgradePreflightRoot,
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
            Self::MissingPowerCommand => {
                write!(
                    f,
                    "missing power command; expected `power on` or `power off`"
                )
            }
            Self::UnknownPowerCommand(command) => {
                write!(f, "unknown power command `{command}`")
            }
            Self::MissingScreenCommand => {
                write!(
                    f,
                    "missing screen command; expected `screen off` or `screen on`"
                )
            }
            Self::UnknownScreenCommand(command) => {
                write!(f, "unknown screen command `{command}`")
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
            Self::UnknownVolumeCommand(command) => {
                write!(f, "unknown volume command `{command}`")
            }
            Self::UnknownMuteCommand(command) => {
                write!(f, "unknown mute command `{command}`; expected `volume mute`, `volume mute on`, or `volume mute off`")
            }
            Self::InvalidVolumeValue(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::MissingUpgradePreflightRoot => {
                write!(f, "missing candidate root for `upgrade-preflight`")
            }
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
    TvClientBuild(TvClientBuildError),
    ConfigPath(ConfigPathError),
    Config(ConfigError),
    StateDir(StateDirError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
    Dev(DevError),
    Settings(SettingsError),
    Updates(UpdatesError),
    UpgradePreflight(CompatibilityReport),
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
            Self::TvClientBuild(err) => write!(f, "{err}"),
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::StateDir(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::UpgradePreflight(report) => write!(f, "{report}"),
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
            Self::TvClientBuild(err) => Some(err),
            Self::ConfigPath(err) => Some(err),
            Self::Config(err) => Some(err),
            Self::StateDir(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
            Self::Dev(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::Updates(err) => Some(err),
            Self::UpgradePreflight(_) => None,
            Self::NotificationAfterPrimary { primary, .. } => Some(primary.as_ref()),
        }
    }
}

impl ParseError {
    pub fn help_topic(&self) -> HelpTopic {
        match self {
            Self::UnknownBrightnessCommand(_)
            | Self::MissingBrightnessValue
            | Self::InvalidBrightnessValue(_) => HelpTopic::Brightness,
            Self::UnexpectedArguments {
                command: Command::Brightness(_),
                ..
            } => HelpTopic::Brightness,
            Self::UnknownVolumeCommand(_)
            | Self::UnknownMuteCommand(_)
            | Self::InvalidVolumeValue(_) => HelpTopic::Volume,
            Self::UnexpectedArguments {
                command: Command::Volume(_),
                ..
            } => HelpTopic::Volume,
            Self::MissingPowerCommand | Self::UnknownPowerCommand(_) => HelpTopic::Power,
            Self::UnexpectedArguments {
                command: Command::Power(_),
                ..
            } => HelpTopic::Power,
            Self::MissingScreenCommand | Self::UnknownScreenCommand(_) => HelpTopic::Screen,
            Self::UnexpectedArguments {
                command: Command::Screen(_),
                ..
            } => HelpTopic::Screen,
            Self::Settings(error) => HelpTopic::Settings(match error {
                SettingsParseError::MissingKey { subcommand }
                | SettingsParseError::MissingValue { subcommand }
                | SettingsParseError::UnexpectedArguments { subcommand, .. } => {
                    SettingsHelpTopic::from_subcommand(subcommand)
                        .unwrap_or(SettingsHelpTopic::Root)
                }
                SettingsParseError::MissingSubcommand
                | SettingsParseError::UnknownSubcommand(_) => SettingsHelpTopic::Root,
            }),
            Self::Updates(error) => HelpTopic::Updates(match error {
                UpdatesParseError::DuplicateNotify => UpdatesHelpTopic::Check,
                UpdatesParseError::UnexpectedArguments { subcommand, .. } => {
                    UpdatesHelpTopic::from_subcommand(subcommand).unwrap_or(UpdatesHelpTopic::Root)
                }
                UpdatesParseError::MissingSubcommand | UpdatesParseError::UnknownSubcommand(_) => {
                    UpdatesHelpTopic::Root
                }
            }),
            _ => HelpTopic::Global,
        }
    }
}

impl From<io::Error> for RunError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<TvClientBuildError> for RunError {
    fn from(value: TvClientBuildError) -> Self {
        Self::TvClientBuild(value)
    }
}

impl Command {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup(_) => "startup",
            Self::Shutdown => "shutdown",
            Self::Power(_) => "power",
            Self::SleepPre => "sleep-pre",
            Self::Sleep => "sleep",
            Self::NetworkManagerPreDown => "nm-pre-down",
            Self::Brightness(_) => "brightness",
            Self::Volume(_) => "volume",
            Self::Screen(_) => "screen",
            Self::ScreenOff => "screen-off",
            Self::ScreenOn => "screen-on",
            Self::Monitor => "monitor",
            Self::Lifecycle => "lifecycle",
            Self::DetectBackend => "detect-backend",
            Self::Dev(command) => command.as_str(),
            Self::Settings(_) => "settings",
            Self::Updates(_) => "updates",
            Self::UpgradePreflight { .. } => "upgrade-preflight",
        }
    }

    pub fn placeholder_message(&self) -> &'static str {
        match self {
            Self::Startup(_) => "TODO: implemented via command handler",
            Self::Shutdown => "TODO: implemented via command handler",
            Self::Power(_) => "TODO: implemented via command handler",
            Self::SleepPre => "TODO: implemented via command handler",
            Self::Sleep => "TODO: implemented via command handler",
            Self::NetworkManagerPreDown => "TODO: implemented via command handler",
            Self::Brightness(_) => "TODO: implemented via command handler",
            Self::Volume(_) => "TODO: implemented via command handler",
            Self::Screen(_) => "TODO: implemented via command handler",
            Self::ScreenOff => "TODO: implemented via command handler",
            Self::ScreenOn => "TODO: implemented via command handler",
            Self::Monitor => "TODO: implemented via command handler",
            Self::Lifecycle => "TODO: implemented via command handler",
            Self::DetectBackend => "TODO: implement detect-backend command",
            Self::Dev(_) => "TODO: implemented via temporary dev command handler",
            Self::Settings(_) => "TODO: implemented via command handler",
            Self::Updates(_) => "TODO: implemented via command handler",
            Self::UpgradePreflight { .. } => "TODO: implemented via command handler",
        }
    }
}

pub fn usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV control

Usage:
  {program} <command>
  {program} help [COMMAND...]
  {program} --help, -h
  {program} --version, -V

Commands:
  brightness      Open the TV brightness control dialog
  brightness get  Print the current TV OLED brightness
  brightness set <0-100>
                  Set the TV OLED brightness
  volume          Print the current TV volume or mute state
  volume <0-100>  Set the TV volume and unmute it
  volume up       Increase the TV volume and unmute it
  volume down     Decrease the TV volume and unmute it
  volume mute     Toggle TV mute
  volume mute on  Mute the TV
  volume mute off Unmute the TV
  power on        Start or restore the TV output
  power off       Power off the TV when LG Buddy owns the active input
  screen off      Blank the configured TV output if active
  screen on       Restore the TV output after an LG Buddy screen blank
  settings        Inspect and edit structured LG Buddy settings
  updates         Check for LG Buddy releases
  help [COMMAND...]
                  Show global or scoped command help

Settings:
  settings list
  settings describe [KEY]
  settings get <KEY>
  settings set <KEY> <VALUE>
  settings unset <KEY>

Updates:
  updates check [--notify]
"
    )
}

pub fn power_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV power control

Usage:
  {program} power on
  {program} power off
  {program} power --help

Commands:
  on              Start or restore the TV output
  off             Power off the TV when LG Buddy owns the active input
"
    )
}

pub fn brightness_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV brightness control

Usage:
  {program} brightness
  {program} brightness get
  {program} brightness set <0-100>
  {program} brightness --help

Commands:
  get             Print the current TV OLED brightness
  set <0-100>     Set the TV OLED brightness
"
    )
}

pub fn volume_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV volume control

Usage:
  {program} volume
  {program} volume <0-100>
  {program} volume up
  {program} volume down
  {program} volume mute
  {program} volume mute on
  {program} volume mute off
  {program} volume --help

Commands:
  <0-100>         Set the TV volume and unmute it
  up              Increase the TV volume and unmute it
  down            Decrease the TV volume and unmute it
  mute            Toggle TV mute
  mute on         Mute the TV
  mute off        Unmute the TV
"
    )
}

pub fn screen_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV screen control

Usage:
  {program} screen off
  {program} screen on
  {program} screen --help

Commands:
  off             Blank the configured TV output if active
  on              Restore the TV output after an LG Buddy screen blank
"
    )
}

pub fn settings_usage(program: &str, topic: SettingsHelpTopic) -> String {
    match topic {
        SettingsHelpTopic::Root => format!(
            "\
LG Buddy settings

Usage:
  {program} settings list
  {program} settings describe [KEY]
  {program} settings get <KEY>
  {program} settings set <KEY> <VALUE>
  {program} settings unset <KEY>
  {program} settings --help

Commands:
  list                    List settings and their effective values
  describe [KEY]          Describe one setting or the complete registry
  get <KEY>               Print one raw effective value
  set <KEY> <VALUE>       Save a setting value
  unset <KEY>             Remove a saved override
"
        ),
        SettingsHelpTopic::List => format!(
            "\
Usage:
  {program} settings list
"
        ),
        SettingsHelpTopic::Describe => format!(
            "\
Usage:
  {program} settings describe [KEY]
"
        ),
        SettingsHelpTopic::Get => format!(
            "\
Usage:
  {program} settings get <KEY>
"
        ),
        SettingsHelpTopic::Set => format!(
            "\
Usage:
  {program} settings set <KEY> <VALUE>
"
        ),
        SettingsHelpTopic::Unset => format!(
            "\
Usage:
  {program} settings unset <KEY>
"
        ),
    }
}

pub fn updates_usage(program: &str, topic: UpdatesHelpTopic) -> String {
    match topic {
        UpdatesHelpTopic::Root => format!(
            "\
LG Buddy update checks

Usage:
  {program} updates check [--notify]
  {program} updates --help

Commands:
  check           Check GitHub releases for an available update
"
        ),
        UpdatesHelpTopic::Check => format!(
            "\
LG Buddy update check

Usage:
  {program} updates check [--notify]

Options:
  --notify        Request a desktop notification when an update is available
"
        ),
    }
}

pub fn help(program: &str, topic: HelpTopic) -> String {
    match topic {
        HelpTopic::Global => usage(program),
        HelpTopic::Brightness => brightness_usage(program),
        HelpTopic::Volume => volume_usage(program),
        HelpTopic::Power => power_usage(program),
        HelpTopic::Screen => screen_usage(program),
        HelpTopic::Settings(topic) => settings_usage(program, topic),
        HelpTopic::Updates(topic) => updates_usage(program, topic),
    }
}

pub fn parse_args<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(ParseOutcome::Help(HelpTopic::Global));
    };

    let first = first.as_ref();
    if matches!(first, "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Global));
    }
    if first == "help" {
        return parse_help_command(args);
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
        "settings" => return parse_settings_command(args),
        "updates" => return parse_updates_command(args),
        "upgrade-preflight" => {
            let candidate_root = PathBuf::from(
                args.next()
                    .ok_or(ParseError::MissingUpgradePreflightRoot)?
                    .as_ref(),
            );
            let mut repair_python = false;
            let mut unexpected = Vec::new();
            for argument in args {
                if argument.as_ref() == "--repair-python" && !repair_python {
                    repair_python = true;
                } else {
                    unexpected.push(argument.as_ref().to_string());
                }
            }
            let command = Command::UpgradePreflight {
                candidate_root,
                repair_python,
            };
            if !unexpected.is_empty() {
                return Err(ParseError::UnexpectedArguments {
                    command,
                    arguments: unexpected,
                });
            }
            return Ok(ParseOutcome::Command(command));
        }
        "dev" => {
            return DevCommand::parse(args)
                .map(|command| ParseOutcome::Command(Command::Dev(command)))
                .map_err(ParseError::Dev);
        }
        "brightness" => return parse_brightness_command(args),
        "volume" => return parse_volume_command(args),
        "power" => return parse_power_command(args),
        "screen" => return parse_screen_command(args),
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
        Command::Power(PowerCommand::On) => crate::commands::run_startup(writer, StartupMode::Boot),
        Command::Power(PowerCommand::Off) => run_shutdown(writer),
        Command::SleepPre => run_sleep_pre(writer),
        Command::Sleep => run_sleep(writer),
        Command::NetworkManagerPreDown => run_nm_pre_down(writer),
        Command::Brightness(command) => run_brightness(writer, command),
        Command::Volume(command) => run_volume(writer, command),
        Command::DetectBackend => run_detect_backend(writer),
        Command::Screen(ScreenCommand::Off) => run_screen_off(writer),
        Command::Screen(ScreenCommand::On) => run_screen_on(writer),
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
        Command::UpgradePreflight {
            candidate_root,
            repair_python,
        } => {
            let report =
                crate::upgrade_preflight::candidate_host_preflight(&candidate_root, repair_python);
            if report.compatible() {
                write!(writer, "{report}")?;
                Ok(())
            } else {
                Err(RunError::UpgradePreflight(report))
            }
        }
    }
}

fn parse_help_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(topic) = args.next() else {
        return Ok(ParseOutcome::Help(HelpTopic::Global));
    };

    let remaining = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();
    match topic.as_ref() {
        "brightness" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "get" | "set"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
            } else {
                Err(ParseError::UnknownBrightnessCommand(remaining.join(" ")))
            }
        }
        "volume" => {
            let valid_topic = remaining.is_empty()
                || (remaining.len() == 1
                    && matches!(remaining[0].as_str(), "up" | "down" | "mute"))
                || (remaining.len() == 2
                    && remaining[0] == "mute"
                    && matches!(remaining[1].as_str(), "on" | "off"));
            if valid_topic {
                Ok(ParseOutcome::Help(HelpTopic::Volume))
            } else {
                Err(ParseError::UnknownVolumeCommand(remaining.join(" ")))
            }
        }
        "power" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "on" | "off"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Power))
            } else {
                Err(ParseError::UnknownPowerCommand(remaining.join(" ")))
            }
        }
        "screen" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "off" | "on"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Screen))
            } else {
                Err(ParseError::UnknownScreenCommand(remaining.join(" ")))
            }
        }
        "settings" => match remaining.as_slice() {
            [] => Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root,
            ))),
            [subcommand] => SettingsHelpTopic::from_subcommand(subcommand)
                .map(|topic| ParseOutcome::Help(HelpTopic::Settings(topic)))
                .ok_or_else(|| {
                    ParseError::Settings(SettingsParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    ))
                }),
            [subcommand, arguments @ ..] => {
                if let Some(topic) = SettingsHelpTopic::from_subcommand(subcommand) {
                    Err(ParseError::Settings(
                        SettingsParseError::UnexpectedArguments {
                            subcommand: settings_help_subcommand(topic),
                            arguments: arguments.to_vec(),
                        },
                    ))
                } else {
                    Err(ParseError::Settings(SettingsParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    )))
                }
            }
        },
        "updates" => match remaining.as_slice() {
            [] => Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root,
            ))),
            [subcommand] => UpdatesHelpTopic::from_subcommand(subcommand)
                .map(|topic| ParseOutcome::Help(HelpTopic::Updates(topic)))
                .ok_or_else(|| {
                    ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    ))
                }),
            [subcommand, arguments @ ..] => {
                if UpdatesHelpTopic::from_subcommand(subcommand).is_some() {
                    Err(ParseError::Updates(
                        UpdatesParseError::UnexpectedArguments {
                            subcommand: "check",
                            arguments: arguments.to_vec(),
                        },
                    ))
                } else {
                    Err(ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    )))
                }
            }
        },
        other => Err(ParseError::UnknownCommand(other.to_string())),
    }
}

fn settings_help_subcommand(topic: SettingsHelpTopic) -> &'static str {
    match topic {
        SettingsHelpTopic::Root => "settings",
        SettingsHelpTopic::List => "list",
        SettingsHelpTopic::Describe => "describe",
        SettingsHelpTopic::Get => "get",
        SettingsHelpTopic::Set => "set",
        SettingsHelpTopic::Unset => "unset",
    }
}

fn parse_settings_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments = args
        .into_iter()
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    let Some(subcommand) = arguments.first() else {
        return Err(ParseError::Settings(SettingsParseError::MissingSubcommand));
    };

    if matches!(subcommand.as_str(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Settings(
            SettingsHelpTopic::Root,
        )));
    }

    if let Some(topic) = SettingsHelpTopic::from_subcommand(subcommand) {
        if arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
        {
            return Ok(ParseOutcome::Help(HelpTopic::Settings(topic)));
        }
    }

    SettingsCommand::parse(arguments)
        .map(|command| ParseOutcome::Command(Command::Settings(command)))
        .map_err(ParseError::Settings)
}

fn parse_updates_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments = args
        .into_iter()
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    let Some(subcommand) = arguments.first() else {
        return Err(ParseError::Updates(UpdatesParseError::MissingSubcommand));
    };

    if matches!(subcommand.as_str(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Updates(
            UpdatesHelpTopic::Root,
        )));
    }

    if UpdatesHelpTopic::from_subcommand(subcommand).is_some()
        && arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(ParseOutcome::Help(HelpTopic::Updates(
            UpdatesHelpTopic::Check,
        )));
    }

    UpdatesCommand::parse(arguments)
        .map(|command| ParseOutcome::Command(Command::Updates(command)))
        .map_err(ParseError::Updates)
}

fn parse_power_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Err(ParseError::MissingPowerCommand);
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Power));
    }

    let command = match subcommand.as_ref() {
        "on" => PowerCommand::On,
        "off" => PowerCommand::Off,
        other => return Err(ParseError::UnknownPowerCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Power(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Power))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Power(command),
            arguments,
        })
    }
}

fn parse_screen_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Err(ParseError::MissingScreenCommand);
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Screen));
    }

    let command = match subcommand.as_ref() {
        "off" => ScreenCommand::Off,
        "on" => ScreenCommand::On,
        other => return Err(ParseError::UnknownScreenCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Screen(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Screen))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Screen(command),
            arguments,
        })
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

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Brightness));
    }

    match subcommand.as_ref() {
        "get" => {
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(
                    BrightnessCommand::Get,
                )))
            } else if extra_args.len() == 1 && matches!(extra_args[0].as_str(), "-h" | "--help") {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
            } else {
                Err(ParseError::UnexpectedArguments {
                    command: Command::Brightness(BrightnessCommand::Get),
                    arguments: extra_args,
                })
            }
        }
        "set" => {
            let value = args.next().ok_or(ParseError::MissingBrightnessValue)?;
            if matches!(value.as_ref(), "-h" | "--help") {
                return Ok(ParseOutcome::Help(HelpTopic::Brightness));
            }
            let brightness = OledBrightness::parse(value.as_ref())
                .map_err(ParseError::InvalidBrightnessValue)?;
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            let command = BrightnessCommand::Set(brightness);
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(command)))
            } else if extra_args.len() == 1 && matches!(extra_args[0].as_str(), "-h" | "--help") {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
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

fn parse_volume_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Get)));
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Volume));
    }

    let command = match subcommand.as_ref() {
        "up" => VolumeCommand::Up,
        "down" => VolumeCommand::Down,
        "mute" => return parse_mute_command(args),
        value => {
            VolumeCommand::Set(VolumeLevel::parse(value).map_err(ParseError::InvalidVolumeValue)?)
        }
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Volume(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Volume))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Volume(command),
            arguments,
        })
    }
}

fn parse_mute_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
            MuteCommand::Toggle,
        ))));
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Volume));
    }

    let mute = match subcommand.as_ref() {
        "on" => MuteCommand::On,
        "off" => MuteCommand::Off,
        other => return Err(ParseError::UnknownMuteCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
            mute,
        ))))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Volume))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Volume(VolumeCommand::Mute(mute)),
            arguments,
        })
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
        brightness_usage, parse_args, power_usage, screen_usage, settings_usage, updates_usage,
        usage, volume_usage, BrightnessCommand, Command, DevCommand, DevParseError, HelpTopic,
        MuteCommand, ParseError, ParseOutcome, PowerCommand, ScreenCommand, SettingsHelpTopic,
        StartupMode, UpdatesHelpTopic, VolumeCommand, WebOsControlProbeCommand,
    };
    use crate::settings::{SettingsCommand, SettingsParseError};
    use crate::tv::{OledBrightness, VolumeLevel};
    use crate::updates::{UpdatesCommand, UpdatesParseError};
    use crate::{notifications::NotificationError, RunError};
    use std::error::Error;
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn no_args_prints_help() {
        assert_eq!(
            parse_args(Vec::<String>::new()),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
    }

    #[test]
    fn explicit_help_prints_help() {
        assert_eq!(
            parse_args(["--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["-h"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["help"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["help", "brightness"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["help", "brightness", "set"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "get", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "set", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["help", "volume"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "mute"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "mute", "on"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "set"]),
            Err(ParseError::UnknownVolumeCommand("set".to_string()))
        );
        assert_eq!(
            parse_args(["volume", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["volume", "up", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["volume", "mute", "on", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "power"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["power", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["power", "on", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["help", "screen"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["screen", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["screen", "off", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["help", "settings"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["help", "settings", "set"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Set
            )))
        );
        assert_eq!(
            parse_args(["settings", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["settings", "set", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Set
            )))
        );
        assert_eq!(
            parse_args(["help", "updates"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["help", "updates", "check"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Check
            )))
        );
        assert_eq!(
            parse_args(["updates", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Check
            )))
        );
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
            parse_args(["power", "on"]),
            Ok(ParseOutcome::Command(Command::Power(PowerCommand::On)))
        );
        assert_eq!(
            parse_args(["power", "off"]),
            Ok(ParseOutcome::Command(Command::Power(PowerCommand::Off)))
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
            parse_args(["volume"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Get)))
        );
        assert_eq!(
            parse_args(["volume", "65"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(65),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "0"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(0),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "100"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(100),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "up"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Up)))
        );
        assert_eq!(
            parse_args(["volume", "down"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Down)))
        );
        assert_eq!(
            parse_args(["volume", "mute"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::Toggle,
            ))))
        );
        assert_eq!(
            parse_args(["volume", "mute", "on"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::On,
            ))))
        );
        assert_eq!(
            parse_args(["volume", "mute", "off"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::Off,
            ))))
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
            parse_args(["screen", "off"]),
            Ok(ParseOutcome::Command(Command::Screen(ScreenCommand::Off)))
        );
        assert_eq!(
            parse_args(["screen", "on"]),
            Ok(ParseOutcome::Command(Command::Screen(ScreenCommand::On)))
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
            parse_args(["dev", "webos-control-probe", "power-off"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::PowerOff)
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
                UpdatesCommand::Check { notify: false }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check { notify: true }
            )))
        );
        assert_eq!(
            parse_args(["updates", "background-check"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::BackgroundCheck
            )))
        );
        assert_eq!(
            parse_args(["upgrade-preflight", "/tmp/lg-buddy-candidate"]),
            Ok(ParseOutcome::Command(Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/lg-buddy-candidate"),
                repair_python: false,
            }))
        );
        assert_eq!(
            parse_args([
                "upgrade-preflight",
                "/tmp/lg-buddy-candidate",
                "--repair-python"
            ]),
            Ok(ParseOutcome::Command(Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/lg-buddy-candidate"),
                repair_python: true,
            }))
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
    fn invalid_power_command_is_rejected_with_power_help() {
        assert_eq!(parse_args(["power"]), Err(ParseError::MissingPowerCommand));
        assert_eq!(
            parse_args(["power", "standby"]),
            Err(ParseError::UnknownPowerCommand("standby".to_string()))
        );
        let error = parse_args(["power", "on", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Power(PowerCommand::On),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Power);
    }

    #[test]
    fn invalid_screen_command_is_rejected_with_screen_help() {
        assert_eq!(
            parse_args(["screen"]),
            Err(ParseError::MissingScreenCommand)
        );
        assert_eq!(
            parse_args(["screen", "toggle"]),
            Err(ParseError::UnknownScreenCommand("toggle".to_string()))
        );
        let error = parse_args(["screen", "off", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Screen(ScreenCommand::Off),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Screen);
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
        let error = parse_args(["brightness", "set", "101"]).unwrap_err();
        assert!(matches!(&error, ParseError::InvalidBrightnessValue(_)));
        assert_eq!(error.help_topic(), HelpTopic::Brightness);
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
        assert_eq!(
            ParseError::MissingBrightnessValue.help_topic(),
            HelpTopic::Brightness
        );
    }

    #[test]
    fn invalid_volume_command_is_rejected_with_volume_help() {
        let error = parse_args(["volume", "101"]).unwrap_err();
        assert!(matches!(&error, ParseError::InvalidVolumeValue(_)));
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        assert!(matches!(
            parse_args(["volume", "abc"]),
            Err(ParseError::InvalidVolumeValue(_))
        ));
        assert_eq!(
            parse_args(["volume", "set", "65"]),
            Err(ParseError::InvalidVolumeValue(
                VolumeLevel::parse("set").unwrap_err()
            ))
        );
        assert_eq!(
            parse_args(["volume", "mute", "toggle"]),
            Err(ParseError::UnknownMuteCommand("toggle".to_string()))
        );
        let error = parse_args(["volume", "up", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Volume(VolumeCommand::Up),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        let error = parse_args(["volume", "mute", "on", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Volume(VolumeCommand::Mute(MuteCommand::On)),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        assert_eq!(
            ParseError::UnknownVolumeCommand("set".to_string()).help_topic(),
            HelpTopic::Volume
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
        let error = parse_args(["settings", "set", "screen.backend"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::Settings(SettingsParseError::MissingValue { subcommand: "set" })
        );
        assert_eq!(
            error.help_topic(),
            HelpTopic::Settings(SettingsHelpTopic::Set)
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
        let error = parse_args(["updates", "check", "--channel"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::Updates(UpdatesParseError::UnexpectedArguments {
                subcommand: "check",
                arguments: vec!["--channel".to_string()]
            })
        );
        assert_eq!(
            error.help_topic(),
            HelpTopic::Updates(UpdatesHelpTopic::Check)
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "stable"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: vec!["--channel".to_string(), "stable".to_string()]
                }
            ))
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
    fn invalid_upgrade_preflight_command_is_rejected() {
        assert_eq!(
            parse_args(["upgrade-preflight"]),
            Err(ParseError::MissingUpgradePreflightRoot)
        );
        assert_eq!(
            parse_args(["upgrade-preflight", "/tmp/candidate", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::UpgradePreflight {
                    candidate_root: PathBuf::from("/tmp/candidate"),
                    repair_python: false,
                },
                arguments: vec!["extra".to_string()],
            })
        );
        assert_eq!(
            parse_args([
                "upgrade-preflight",
                "/tmp/candidate",
                "--repair-python",
                "--repair-python"
            ]),
            Err(ParseError::UnexpectedArguments {
                command: Command::UpgradePreflight {
                    candidate_root: PathBuf::from("/tmp/candidate"),
                    repair_python: true,
                },
                arguments: vec!["--repair-python".to_string()],
            })
        );
    }

    #[test]
    fn global_usage_only_mentions_public_commands() {
        let help = usage("lg-buddy");

        for command in [
            "brightness",
            "brightness get",
            "brightness set <0-100>",
            "volume",
            "volume <0-100>",
            "volume up",
            "volume down",
            "volume mute",
            "volume mute on",
            "volume mute off",
            "power on",
            "power off",
            "screen off",
            "screen on",
            "settings",
            "updates",
            "help [COMMAND...]",
        ] {
            assert!(
                help.contains(command),
                "missing `{command}` from help output"
            );
        }
        for command in [
            "startup",
            "shutdown",
            "sleep-pre",
            "\n  sleep ",
            "nm-pre-down",
            "monitor",
            "lifecycle",
            "\n  dev ",
            "screen-off",
            "screen-on",
            "detect-backend",
            "updates background-check",
            "upgrade-preflight",
            "webos-auth-probe",
            "webos-read-probe",
        ] {
            assert!(
                !help.contains(command),
                "package-owned entrypoint `{command}` leaked into global help"
            );
        }
    }

    #[test]
    fn power_usage_mentions_public_commands() {
        let help = power_usage("lg-buddy");

        assert!(help.contains("lg-buddy power on"));
        assert!(help.contains("lg-buddy power off"));
        assert!(!help.contains("startup"));
        assert!(!help.contains("shutdown"));
    }

    #[test]
    fn brightness_usage_mentions_public_commands() {
        let help = brightness_usage("lg-buddy");

        assert!(help.contains("lg-buddy brightness\n"));
        assert!(help.contains("lg-buddy brightness get"));
        assert!(help.contains("lg-buddy brightness set <0-100>"));
    }

    #[test]
    fn volume_usage_mentions_public_commands() {
        let help = volume_usage("lg-buddy");

        for command in [
            "lg-buddy volume\n",
            "lg-buddy volume <0-100>",
            "lg-buddy volume up",
            "lg-buddy volume down",
            "lg-buddy volume mute",
            "lg-buddy volume mute on",
            "lg-buddy volume mute off",
        ] {
            assert!(help.contains(command), "missing `{command}` from help");
        }
        assert!(!help.contains("volume set"));
    }

    #[test]
    fn screen_usage_mentions_public_commands() {
        let help = screen_usage("lg-buddy");

        assert!(help.contains("lg-buddy screen off"));
        assert!(help.contains("lg-buddy screen on"));
        assert!(!help.contains("screen-off"));
        assert!(!help.contains("screen-on"));
    }

    #[test]
    fn settings_usage_is_scoped_to_the_requested_level() {
        let root = settings_usage("lg-buddy", SettingsHelpTopic::Root);
        assert!(root.contains("lg-buddy settings list"));
        assert!(root.contains("lg-buddy settings describe [KEY]"));
        assert!(root.contains("set <KEY> <VALUE>"));

        let set = settings_usage("lg-buddy", SettingsHelpTopic::Set);
        assert!(set.contains("lg-buddy settings set <KEY> <VALUE>"));
        assert!(!set.contains("settings list"));
    }

    #[test]
    fn updates_usage_is_scoped_and_hides_the_timer_entrypoint() {
        let root = updates_usage("lg-buddy", UpdatesHelpTopic::Root);
        assert!(root.contains("lg-buddy updates check [--notify]"));
        assert!(!root.contains("--channel"));
        assert!(!root.contains("background-check"));

        let check = updates_usage("lg-buddy", UpdatesHelpTopic::Check);
        assert!(!check.contains("--channel"));
        assert!(check.contains("--notify"));
        assert!(!check.contains("background-check"));
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
            "settings describe [KEY]",
            "settings get <KEY>",
            "settings set <KEY> <VALUE>",
            "settings unset <KEY>",
            "updates check [--notify]",
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

    fn volume(value: u8) -> VolumeLevel {
        VolumeLevel::new(value).expect("test volume should be valid")
    }
}
