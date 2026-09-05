use crate::auth::{resolve_config_owner, AuthContextError, SystemUser};
use crate::config::{load_config, resolve_config_path_from_env, ConfigError, ConfigPathError};
use crate::platform_access_token::{PlatformAccessTokenStore, PlatformAccessTokenStoreError};
use crate::web_os::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsBacklightBrightness,
    WebOsBacklightBrightnessError, WebOsClient, WebOsControlError, WebOsEndpoint,
    WebOsForegroundApp, WebOsForegroundAppError, WebOsInputId, WebOsInputIdError, WebOsPowerState,
    WebOsPowerStateError, WebOsScreenControlError, WebOsSetBacklightBrightnessError,
};
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

const WEBOS_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const WEBOS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const WEBOS_AUTH_PROBE_PREFIX: &str = "LG Buddy WebOS Auth Probe";
const WEBOS_READ_PROBE_PREFIX: &str = "LG Buddy WebOS Read Probe";
const WEBOS_CONTROL_PROBE_PREFIX: &str = "LG Buddy WebOS Control Probe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsControlProbeCommand {
    SetInput,
    SetBacklight(WebOsBacklightBrightness),
    ScreenOff,
    ScreenOn,
    PowerOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevCommand {
    WebOsAuthProbe,
    WebOsReadProbe,
    WebOsControlProbe(WebOsControlProbeCommand),
}

impl DevCommand {
    pub(crate) fn parse<I, S>(args: I) -> Result<Self, DevParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let subcommand = args.next().ok_or(DevParseError::MissingSubcommand)?;
        let command = match subcommand.as_ref() {
            "webos-auth-probe" => Self::WebOsAuthProbe,
            "webos-read-probe" => Self::WebOsReadProbe,
            "webos-control-probe" => {
                let operation = args.next().ok_or(DevParseError::MissingControlOperation)?;
                let operation = match operation.as_ref() {
                    "set-input" => WebOsControlProbeCommand::SetInput,
                    "set-backlight" => {
                        let value = args
                            .next()
                            .ok_or(DevParseError::MissingBacklightBrightness)?;
                        let value = value.as_ref();
                        let brightness = value
                            .parse::<u8>()
                            .ok()
                            .and_then(|value| WebOsBacklightBrightness::new(value).ok())
                            .ok_or_else(|| {
                                DevParseError::InvalidBacklightBrightness(value.to_string())
                            })?;
                        WebOsControlProbeCommand::SetBacklight(brightness)
                    }
                    "screen-off" => WebOsControlProbeCommand::ScreenOff,
                    "screen-on" => WebOsControlProbeCommand::ScreenOn,
                    "power-off" => WebOsControlProbeCommand::PowerOff,
                    other => return Err(DevParseError::UnknownControlOperation(other.to_string())),
                };
                Self::WebOsControlProbe(operation)
            }
            other => return Err(DevParseError::UnknownSubcommand(other.to_string())),
        };

        let arguments: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
        if !arguments.is_empty() {
            return Err(DevParseError::UnexpectedArguments { command, arguments });
        }

        Ok(command)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WebOsAuthProbe => "dev webos-auth-probe",
            Self::WebOsReadProbe => "dev webos-read-probe",
            Self::WebOsControlProbe(WebOsControlProbeCommand::SetInput) => {
                "dev webos-control-probe set-input"
            }
            Self::WebOsControlProbe(WebOsControlProbeCommand::SetBacklight(_)) => {
                "dev webos-control-probe set-backlight"
            }
            Self::WebOsControlProbe(WebOsControlProbeCommand::ScreenOff) => {
                "dev webos-control-probe screen-off"
            }
            Self::WebOsControlProbe(WebOsControlProbeCommand::ScreenOn) => {
                "dev webos-control-probe screen-on"
            }
            Self::WebOsControlProbe(WebOsControlProbeCommand::PowerOff) => {
                "dev webos-control-probe power-off"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingControlOperation,
    MissingBacklightBrightness,
    InvalidBacklightBrightness(String),
    UnknownControlOperation(String),
    UnexpectedArguments {
        command: DevCommand,
        arguments: Vec<String>,
    },
}

impl fmt::Display for DevParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => write!(f, "missing temporary `dev` subcommand"),
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown temporary dev command `{subcommand}`")
            }
            Self::MissingControlOperation => {
                write!(f, "missing temporary webOS control probe operation")
            }
            Self::MissingBacklightBrightness => {
                write!(f, "missing temporary webOS backlight brightness")
            }
            Self::InvalidBacklightBrightness(value) => write!(
                f,
                "invalid temporary webOS backlight brightness `{value}`; expected 0-100"
            ),
            Self::UnknownControlOperation(operation) => {
                write!(f, "unknown temporary webOS control operation `{operation}`")
            }
            Self::UnexpectedArguments { command, arguments } => write!(
                f,
                "unexpected arguments for `{}`: {}",
                command.as_str(),
                arguments.join(" ")
            ),
        }
    }
}

impl Error for DevParseError {}

#[derive(Debug)]
pub enum DevError {
    Io(io::Error),
    ConfigPath(ConfigPathError),
    Config(ConfigError),
    ConfigOwner(AuthContextError),
    TokenStore(PlatformAccessTokenStoreError),
    Authentication(WebOsAuthenticatedClientError),
    PowerState(WebOsPowerStateError),
    ForegroundApp(WebOsForegroundAppError),
    BacklightBrightness(WebOsBacklightBrightnessError),
    SetBacklightBrightness(WebOsSetBacklightBrightnessError),
    InputId(WebOsInputIdError),
    Control(WebOsControlError),
    ScreenControl(WebOsScreenControlError),
    PowerOffPrecondition { actual: WebOsPowerState },
    BacklightWritePrecondition { actual: WebOsPowerState },
}

impl fmt::Display for DevError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "could not write webOS probe output: {source}"),
            Self::ConfigPath(source) => write!(f, "could not resolve probe config: {source}"),
            Self::Config(source) => write!(f, "could not load probe config: {source}"),
            Self::ConfigOwner(source) => {
                write!(f, "could not resolve probe config owner: {source}")
            }
            Self::TokenStore(source) => {
                write!(f, "could not construct probe access-token store: {source}")
            }
            Self::Authentication(source) => {
                write!(f, "webOS probe authentication failed: {source}")
            }
            Self::PowerState(source) => write!(f, "webOS probe power-state read failed: {source}"),
            Self::ForegroundApp(source) => {
                write!(f, "webOS read probe foreground-app read failed: {source}")
            }
            Self::BacklightBrightness(source) => {
                write!(f, "webOS read probe backlight read failed: {source}")
            }
            Self::SetBacklightBrightness(source) => {
                write!(f, "webOS control probe backlight write failed: {source}")
            }
            Self::InputId(source) => write!(f, "invalid configured webOS input: {source}"),
            Self::Control(source) => write!(f, "webOS control probe failed: {source}"),
            Self::ScreenControl(source) => {
                write!(f, "webOS screen-control probe failed: {source}")
            }
            Self::PowerOffPrecondition { actual } => write!(
                f,
                "refusing webOS power-off probe from power state {actual}; expected Active"
            ),
            Self::BacklightWritePrecondition { actual } => write!(
                f,
                "refusing webOS backlight-write probe from power state {actual}; expected Active"
            ),
        }
    }
}

impl Error for DevError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::ConfigPath(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::ConfigOwner(source) => Some(source),
            Self::TokenStore(source) => Some(source),
            Self::Authentication(source) => Some(source),
            Self::PowerState(source) => Some(source),
            Self::ForegroundApp(source) => Some(source),
            Self::BacklightBrightness(source) => Some(source),
            Self::SetBacklightBrightness(source) => Some(source),
            Self::InputId(source) => Some(source),
            Self::Control(source) => Some(source),
            Self::ScreenControl(source) => Some(source),
            Self::PowerOffPrecondition { .. } | Self::BacklightWritePrecondition { .. } => None,
        }
    }
}

pub(crate) fn run_dev_command<W: Write>(
    command: DevCommand,
    writer: &mut W,
) -> Result<(), DevError> {
    match command {
        DevCommand::WebOsAuthProbe => run_webos_auth_probe(writer),
        DevCommand::WebOsReadProbe => run_webos_read_probe(writer),
        DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput) => {
            run_webos_set_input_probe(writer)
        }
        DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetBacklight(brightness)) => {
            run_webos_set_backlight_probe(writer, brightness)
        }
        DevCommand::WebOsControlProbe(
            operation @ (WebOsControlProbeCommand::ScreenOff | WebOsControlProbeCommand::ScreenOn),
        ) => run_webos_screen_control_probe(writer, operation),
        DevCommand::WebOsControlProbe(WebOsControlProbeCommand::PowerOff) => {
            run_webos_power_off_probe(writer)
        }
    }
}

fn run_webos_auth_probe<W: Write>(writer: &mut W) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;

    run_webos_auth_probe_with(writer, &context, |endpoint, token_store, on_auth_event| {
        let mut client = WebOsClient::connect_authenticated(
            endpoint,
            WEBOS_CONNECT_TIMEOUT,
            WEBOS_RESPONSE_TIMEOUT,
            token_store,
            on_auth_event,
        )
        .map_err(DevError::Authentication)?;
        client.power_state().map_err(DevError::PowerState)
    })
}

fn run_webos_read_probe<W: Write>(writer: &mut W) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;

    run_webos_read_probe_with(writer, &context, |endpoint, token_store, on_auth_event| {
        let mut client = WebOsClient::connect_authenticated(
            endpoint,
            WEBOS_CONNECT_TIMEOUT,
            WEBOS_RESPONSE_TIMEOUT,
            token_store,
            on_auth_event,
        )
        .map_err(DevError::Authentication)?;
        let power_state = client.power_state().map_err(DevError::PowerState)?;
        let foreground_app = client.foreground_app().map_err(DevError::ForegroundApp)?;
        let backlight_brightness = client
            .backlight_brightness()
            .map_err(DevError::BacklightBrightness)?;
        Ok(WebOsReadProbeResult {
            power_state,
            foreground_app,
            backlight_brightness,
        })
    })
}

fn run_webos_set_input_probe<W: Write>(writer: &mut W) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;
    let input_id = WebOsInputId::new(config.input.as_str()).map_err(DevError::InputId)?;

    run_webos_set_input_probe_with(
        writer,
        &context,
        &input_id,
        |endpoint, token_store, on_auth_event, input_id| {
            let mut client = WebOsClient::connect_authenticated(
                endpoint,
                WEBOS_CONNECT_TIMEOUT,
                WEBOS_RESPONSE_TIMEOUT,
                token_store,
                on_auth_event,
            )
            .map_err(DevError::Authentication)?;
            client.switch_input(input_id).map_err(DevError::Control)
        },
    )
}

fn run_webos_screen_control_probe<W: Write>(
    writer: &mut W,
    operation: WebOsControlProbeCommand,
) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;

    run_webos_screen_control_probe_with(
        writer,
        &context,
        operation,
        |endpoint, token_store, on_auth_event, operation| {
            let mut client = WebOsClient::connect_authenticated(
                endpoint,
                WEBOS_CONNECT_TIMEOUT,
                WEBOS_RESPONSE_TIMEOUT,
                token_store,
                on_auth_event,
            )
            .map_err(DevError::Authentication)?;
            match operation {
                WebOsControlProbeCommand::ScreenOff => client.turn_screen_off(),
                WebOsControlProbeCommand::ScreenOn => client.turn_screen_on(),
                WebOsControlProbeCommand::SetInput
                | WebOsControlProbeCommand::SetBacklight(_)
                | WebOsControlProbeCommand::PowerOff => {
                    unreachable!("screen probe operation")
                }
            }
            .map_err(DevError::ScreenControl)
        },
    )
}

fn run_webos_power_off_probe<W: Write>(writer: &mut W) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;

    run_webos_power_off_probe_with(writer, &context, |endpoint, token_store, on_auth_event| {
        let mut client = WebOsClient::connect_authenticated(
            endpoint,
            WEBOS_CONNECT_TIMEOUT,
            WEBOS_RESPONSE_TIMEOUT,
            token_store,
            on_auth_event,
        )
        .map_err(DevError::Authentication)?;
        let power_state = client.power_state().map_err(DevError::PowerState)?;
        require_power_off_state(&power_state)?;
        client.power_off().map_err(DevError::Control)?;
        Ok(power_state)
    })
}

fn run_webos_set_backlight_probe<W: Write>(
    writer: &mut W,
    brightness: WebOsBacklightBrightness,
) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsProbeContext::new(&config_path, config.tv_ip, owner)?;

    run_webos_set_backlight_probe_with(
        writer,
        &context,
        brightness,
        |endpoint, token_store, on_auth_event, brightness| {
            let mut client = WebOsClient::connect_authenticated(
                endpoint,
                WEBOS_CONNECT_TIMEOUT,
                WEBOS_RESPONSE_TIMEOUT,
                token_store,
                on_auth_event,
            )
            .map_err(DevError::Authentication)?;
            let power_state = client.power_state().map_err(DevError::PowerState)?;
            require_active_backlight_write_state(&power_state)?;
            client
                .set_backlight_brightness(brightness)
                .map_err(DevError::SetBacklightBrightness)
        },
    )
}

fn require_power_off_state(power_state: &WebOsPowerState) -> Result<(), DevError> {
    if matches!(
        power_state,
        WebOsPowerState::Active | WebOsPowerState::ScreenOff
    ) {
        Ok(())
    } else {
        Err(DevError::PowerOffPrecondition {
            actual: power_state.clone(),
        })
    }
}

fn require_active_backlight_write_state(power_state: &WebOsPowerState) -> Result<(), DevError> {
    if power_state == &WebOsPowerState::Active {
        Ok(())
    } else {
        Err(DevError::BacklightWritePrecondition {
            actual: power_state.clone(),
        })
    }
}

struct WebOsProbeContext {
    endpoint: WebOsEndpoint,
    token_store: PlatformAccessTokenStore,
}

impl WebOsProbeContext {
    fn new(config_path: &Path, tv_ip: Ipv4Addr, owner: SystemUser) -> Result<Self, DevError> {
        let token_store = PlatformAccessTokenStore::for_primary_profile(config_path, owner)
            .map_err(DevError::TokenStore)?;
        Ok(Self {
            endpoint: WebOsEndpoint::wss(tv_ip),
            token_store,
        })
    }
}

fn run_webos_auth_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
    ) -> Result<WebOsPowerState, DevError>,
{
    let power_state = execute_webos_probe(writer, context, WEBOS_AUTH_PROBE_PREFIX, probe)?;
    writeln!(
        writer,
        "{WEBOS_AUTH_PROBE_PREFIX}: power_state={power_state}"
    )
    .map_err(DevError::Io)
}

struct WebOsReadProbeResult {
    power_state: WebOsPowerState,
    foreground_app: WebOsForegroundApp,
    backlight_brightness: WebOsBacklightBrightness,
}

fn run_webos_read_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
    ) -> Result<WebOsReadProbeResult, DevError>,
{
    let result = execute_webos_probe(writer, context, WEBOS_READ_PROBE_PREFIX, probe)?;
    writeln!(
        writer,
        "{WEBOS_READ_PROBE_PREFIX}: power_state={}",
        result.power_state
    )
    .and_then(|_| {
        writeln!(
            writer,
            "{WEBOS_READ_PROBE_PREFIX}: foreground_app={}",
            result.foreground_app
        )
    })
    .and_then(|_| {
        writeln!(
            writer,
            "{WEBOS_READ_PROBE_PREFIX}: backlight={}",
            result.backlight_brightness
        )
    })
    .map_err(DevError::Io)
}

fn run_webos_set_input_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    input_id: &WebOsInputId,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
        &WebOsInputId,
    ) -> Result<(), DevError>,
{
    execute_webos_probe(
        writer,
        context,
        WEBOS_CONTROL_PROBE_PREFIX,
        |endpoint, token_store, on_auth_event| {
            probe(endpoint, token_store, on_auth_event, input_id)
        },
    )?;
    writeln!(writer, "{WEBOS_CONTROL_PROBE_PREFIX}: input={input_id}").map_err(DevError::Io)
}

fn run_webos_set_backlight_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    brightness: WebOsBacklightBrightness,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
        WebOsBacklightBrightness,
    ) -> Result<(), DevError>,
{
    execute_webos_probe(
        writer,
        context,
        WEBOS_CONTROL_PROBE_PREFIX,
        |endpoint, token_store, on_auth_event| {
            probe(endpoint, token_store, on_auth_event, brightness)
        },
    )?;
    writeln!(
        writer,
        "{WEBOS_CONTROL_PROBE_PREFIX}: backlight={brightness}"
    )
    .map_err(DevError::Io)
}

fn run_webos_screen_control_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    operation: WebOsControlProbeCommand,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
        WebOsControlProbeCommand,
    ) -> Result<WebOsPowerState, DevError>,
{
    let state = execute_webos_probe(
        writer,
        context,
        WEBOS_CONTROL_PROBE_PREFIX,
        |endpoint, token_store, on_auth_event| {
            probe(endpoint, token_store, on_auth_event, operation)
        },
    )?;
    writeln!(writer, "{WEBOS_CONTROL_PROBE_PREFIX}: screen_state={state}").map_err(DevError::Io)
}

fn run_webos_power_off_probe_with<W, F>(
    writer: &mut W,
    context: &WebOsProbeContext,
    probe: F,
) -> Result<(), DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
    ) -> Result<WebOsPowerState, DevError>,
{
    let power_state = execute_webos_probe(writer, context, WEBOS_CONTROL_PROBE_PREFIX, probe)?;
    writeln!(
        writer,
        "{WEBOS_CONTROL_PROBE_PREFIX}: power_off_from={power_state}"
    )
    .map_err(DevError::Io)
}

fn execute_webos_probe<W, F, T>(
    writer: &mut W,
    context: &WebOsProbeContext,
    prefix: &str,
    probe: F,
) -> Result<T, DevError>
where
    W: Write,
    F: FnOnce(
        WebOsEndpoint,
        &PlatformAccessTokenStore,
        &mut dyn FnMut(WebOsAuthenticationEvent),
    ) -> Result<T, DevError>,
{
    let mut progress_error = None;
    let probe_result = {
        let mut on_auth_event = |event| {
            if progress_error.is_none() {
                progress_error =
                    write_auth_event(writer, prefix, context.token_store.token_path(), event).err();
            }
        };
        probe(context.endpoint, &context.token_store, &mut on_auth_event)
    };

    if let Some(source) = progress_error {
        return Err(DevError::Io(source));
    }

    probe_result
}

fn write_auth_event<W: Write>(
    writer: &mut W,
    prefix: &str,
    token_path: &Path,
    event: WebOsAuthenticationEvent,
) -> io::Result<()> {
    match event {
        WebOsAuthenticationEvent::UsingStoredAccessToken => {
            writeln!(writer, "{prefix}: using stored access token.")?;
        }
        WebOsAuthenticationEvent::PairingPrompt => {
            writeln!(
                writer,
                "{prefix}: pairing required; accept the prompt on the TV."
            )?;
        }
        WebOsAuthenticationEvent::AccessTokenPersisted => {
            writeln!(
                writer,
                "{prefix}: stored access token at {}",
                token_path.display()
            )?;
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        require_active_backlight_write_state, require_power_off_state, run_webos_auth_probe_with,
        run_webos_power_off_probe_with, run_webos_read_probe_with,
        run_webos_screen_control_probe_with, run_webos_set_backlight_probe_with,
        run_webos_set_input_probe_with, DevCommand, DevError, DevParseError,
        WebOsControlProbeCommand, WebOsProbeContext, WebOsReadProbeResult,
    };
    use crate::auth::SystemUser;
    use crate::web_os::{
        WebOsAuthenticationEvent, WebOsBacklightBrightness, WebOsForegroundApp, WebOsInputId,
        WebOsPowerState, WebOsPowerStateError,
    };
    use std::net::Ipv4Addr;
    use std::path::Path;

    fn probe_context() -> WebOsProbeContext {
        WebOsProbeContext::new(
            Path::new("/tmp/lg-buddy-dev-probe/config.env"),
            Ipv4Addr::new(192, 0, 2, 42),
            SystemUser::new("test-user", 1000, 1000, "/tmp"),
        )
        .expect("construct probe context")
    }

    #[test]
    fn parser_accepts_only_known_webos_probes() {
        assert_eq!(
            DevCommand::parse(["webos-auth-probe"]),
            Ok(DevCommand::WebOsAuthProbe)
        );
        assert_eq!(
            DevCommand::parse(["webos-read-probe"]),
            Ok(DevCommand::WebOsReadProbe)
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "set-input"]),
            Ok(DevCommand::WebOsControlProbe(
                WebOsControlProbeCommand::SetInput
            ))
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "set-backlight", "75"]),
            Ok(DevCommand::WebOsControlProbe(
                WebOsControlProbeCommand::SetBacklight(
                    WebOsBacklightBrightness::new(75).expect("backlight brightness")
                )
            ))
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "screen-off"]),
            Ok(DevCommand::WebOsControlProbe(
                WebOsControlProbeCommand::ScreenOff
            ))
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "screen-on"]),
            Ok(DevCommand::WebOsControlProbe(
                WebOsControlProbeCommand::ScreenOn
            ))
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "power-off"]),
            Ok(DevCommand::WebOsControlProbe(
                WebOsControlProbeCommand::PowerOff
            ))
        );
        assert_eq!(
            DevCommand::parse(Vec::<String>::new()),
            Err(DevParseError::MissingSubcommand)
        );
        assert_eq!(
            DevCommand::parse(["other"]),
            Err(DevParseError::UnknownSubcommand("other".to_string()))
        );
        assert_eq!(
            DevCommand::parse(["webos-auth-probe", "extra"]),
            Err(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsAuthProbe,
                arguments: vec!["extra".to_string()],
            })
        );
        assert_eq!(
            DevCommand::parse(["webos-read-probe", "extra"]),
            Err(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsReadProbe,
                arguments: vec!["extra".to_string()],
            })
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe"]),
            Err(DevParseError::MissingControlOperation)
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "set-backlight"]),
            Err(DevParseError::MissingBacklightBrightness)
        );
        for value in ["-1", "101", "bright"] {
            assert_eq!(
                DevCommand::parse(["webos-control-probe", "set-backlight", value]),
                Err(DevParseError::InvalidBacklightBrightness(value.to_string()))
            );
        }
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "other"]),
            Err(DevParseError::UnknownControlOperation("other".to_string()))
        );
        assert_eq!(
            DevCommand::parse(["webos-control-probe", "set-input", "extra"]),
            Err(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput),
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn stored_token_probe_reports_reuse_and_state() {
        let context = probe_context();
        let mut output = Vec::new();

        run_webos_auth_probe_with(
            &mut output,
            &context,
            |endpoint, token_store, on_auth_event| {
                assert_eq!(endpoint.to_string(), "wss://192.0.2.42:3001/");
                assert_eq!(
                    token_store.token_path(),
                    Path::new("/tmp/lg-buddy-dev-probe/tvs/primary/access-token.json")
                );
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Ok(WebOsPowerState::Active)
            },
        )
        .expect("run stored-token probe");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Auth Probe: using stored access token.\n\
LG Buddy WebOS Auth Probe: power_state=Active\n"
        );
    }

    #[test]
    fn first_pairing_probe_reports_prompt_persistence_and_state_without_token_value() {
        let context = probe_context();
        let mut output = Vec::new();

        run_webos_auth_probe_with(
            &mut output,
            &context,
            |_endpoint, _token_store, on_auth_event| {
                on_auth_event(WebOsAuthenticationEvent::PairingPrompt);
                on_auth_event(WebOsAuthenticationEvent::AccessTokenPersisted);
                Ok(WebOsPowerState::ActiveStandby)
            },
        )
        .expect("run first-pairing probe");

        let output = String::from_utf8(output).expect("UTF-8 output");
        assert_eq!(
            output,
            "LG Buddy WebOS Auth Probe: pairing required; accept the prompt on the TV.\n\
LG Buddy WebOS Auth Probe: stored access token at /tmp/lg-buddy-dev-probe/tvs/primary/access-token.json\n\
LG Buddy WebOS Auth Probe: power_state=Active Standby\n"
        );
        assert!(!output.contains("client-key"));
    }

    #[test]
    fn read_probe_reports_hardware_observed_values() {
        let context = probe_context();
        let mut output = Vec::new();

        run_webos_read_probe_with(
            &mut output,
            &context,
            |endpoint, token_store, on_auth_event| {
                assert_eq!(endpoint.to_string(), "wss://192.0.2.42:3001/");
                assert_eq!(
                    token_store.token_path(),
                    Path::new("/tmp/lg-buddy-dev-probe/tvs/primary/access-token.json")
                );
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Ok(WebOsReadProbeResult {
                    power_state: WebOsPowerState::Active,
                    foreground_app: WebOsForegroundApp::from_test_app_id("com.webos.app.hdmi3"),
                    backlight_brightness: WebOsBacklightBrightness::new(100)
                        .expect("test backlight brightness"),
                })
            },
        )
        .expect("run read probe");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Read Probe: using stored access token.\n\
LG Buddy WebOS Read Probe: power_state=Active\n\
LG Buddy WebOS Read Probe: foreground_app=com.webos.app.hdmi3\n\
LG Buddy WebOS Read Probe: backlight=100\n"
        );
    }

    #[test]
    fn set_input_probe_reports_configured_input_after_success() {
        let context = probe_context();
        let input_id = WebOsInputId::new("HDMI_3").expect("input ID");
        let mut output = Vec::new();

        run_webos_set_input_probe_with(
            &mut output,
            &context,
            &input_id,
            |endpoint, token_store, on_auth_event, observed_input_id| {
                assert_eq!(endpoint.to_string(), "wss://192.0.2.42:3001/");
                assert_eq!(
                    token_store.token_path(),
                    Path::new("/tmp/lg-buddy-dev-probe/tvs/primary/access-token.json")
                );
                assert_eq!(observed_input_id, &input_id);
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Ok(())
            },
        )
        .expect("run set-input probe");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Control Probe: using stored access token.\n\
LG Buddy WebOS Control Probe: input=HDMI_3\n"
        );
    }

    #[test]
    fn set_backlight_probe_reports_verified_brightness_after_success() {
        let context = probe_context();
        let brightness = WebOsBacklightBrightness::new(75).expect("backlight brightness");
        let mut output = Vec::new();

        run_webos_set_backlight_probe_with(
            &mut output,
            &context,
            brightness,
            |endpoint, token_store, on_auth_event, observed_brightness| {
                assert_eq!(endpoint.to_string(), "wss://192.0.2.42:3001/");
                assert_eq!(
                    token_store.token_path(),
                    Path::new("/tmp/lg-buddy-dev-probe/tvs/primary/access-token.json")
                );
                assert_eq!(observed_brightness, brightness);
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Ok(())
            },
        )
        .expect("run set-backlight probe");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Control Probe: using stored access token.\n\
LG Buddy WebOS Control Probe: backlight=75\n"
        );
    }

    #[test]
    fn screen_control_probe_reports_resulting_state() {
        for (operation, state) in [
            (
                WebOsControlProbeCommand::ScreenOff,
                WebOsPowerState::ScreenOff,
            ),
            (WebOsControlProbeCommand::ScreenOn, WebOsPowerState::Active),
        ] {
            let context = probe_context();
            let mut output = Vec::new();

            run_webos_screen_control_probe_with(
                &mut output,
                &context,
                operation,
                |_endpoint, _token_store, on_auth_event, observed_operation| {
                    assert_eq!(observed_operation, operation);
                    on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                    Ok(state.clone())
                },
            )
            .expect("run screen-control probe");

            assert_eq!(
                String::from_utf8(output).expect("UTF-8 output"),
                format!(
                    "LG Buddy WebOS Control Probe: using stored access token.\n\
LG Buddy WebOS Control Probe: screen_state={state}\n"
                )
            );
        }
    }

    #[test]
    fn power_off_probe_reports_active_precondition_after_success() {
        let context = probe_context();
        let mut output = Vec::new();

        run_webos_power_off_probe_with(
            &mut output,
            &context,
            |_endpoint, _token_store, on_auth_event| {
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Ok(WebOsPowerState::Active)
            },
        )
        .expect("run power-off probe");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Control Probe: using stored access token.\n\
LG Buddy WebOS Control Probe: power_off_from=Active\n"
        );
    }

    #[test]
    fn power_off_probe_accepts_active_and_screen_off_states() {
        assert!(require_power_off_state(&WebOsPowerState::Active).is_ok());
        assert!(require_power_off_state(&WebOsPowerState::ScreenOff).is_ok());

        let error = require_power_off_state(&WebOsPowerState::Suspend)
            .expect_err("suspended TV should not be powered off by the probe");
        assert!(matches!(
            error,
            DevError::PowerOffPrecondition {
                actual: WebOsPowerState::Suspend
            }
        ));
    }

    #[test]
    fn backlight_write_probe_requires_active_power_state() {
        assert!(require_active_backlight_write_state(&WebOsPowerState::Active).is_ok());

        let error = require_active_backlight_write_state(&WebOsPowerState::ScreenOff)
            .expect_err("screen-off TV should not accept the backlight-write probe");
        assert!(matches!(
            error,
            DevError::BacklightWritePrecondition {
                actual: WebOsPowerState::ScreenOff
            }
        ));
    }

    #[test]
    fn typed_probe_failure_is_returned_without_success_output() {
        let context = probe_context();
        let mut output = Vec::new();

        let error = run_webos_auth_probe_with(
            &mut output,
            &context,
            |_endpoint, _token_store, on_auth_event| {
                on_auth_event(WebOsAuthenticationEvent::UsingStoredAccessToken);
                Err(DevError::PowerState(WebOsPowerStateError::MissingPayload))
            },
        )
        .expect_err("probe failure should be returned");

        assert!(error.to_string().contains("has no payload"));
        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "LG Buddy WebOS Auth Probe: using stored access token.\n"
        );
    }
}
