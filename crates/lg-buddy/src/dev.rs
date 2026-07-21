use crate::auth::{resolve_config_owner, AuthContextError, SystemUser};
use crate::config::{load_config, resolve_config_path_from_env, ConfigError, ConfigPathError};
use crate::platform_access_token::{PlatformAccessTokenStore, PlatformAccessTokenStoreError};
use crate::web_os::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsEndpoint,
    WebOsPowerState, WebOsPowerStateError,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevCommand {
    WebOsAuthProbe,
}

impl DevCommand {
    pub(crate) fn parse<I, S>(args: I) -> Result<Self, DevParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let subcommand = args.next().ok_or(DevParseError::MissingSubcommand)?;
        if subcommand.as_ref() != "webos-auth-probe" {
            return Err(DevParseError::UnknownSubcommand(
                subcommand.as_ref().to_string(),
            ));
        }

        let arguments: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
        if !arguments.is_empty() {
            return Err(DevParseError::UnexpectedArguments { arguments });
        }

        Ok(Self::WebOsAuthProbe)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WebOsAuthProbe => "dev webos-auth-probe",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    UnexpectedArguments { arguments: Vec<String> },
}

impl fmt::Display for DevParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => write!(f, "missing temporary `dev` subcommand"),
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown temporary dev command `{subcommand}`")
            }
            Self::UnexpectedArguments { arguments } => write!(
                f,
                "unexpected arguments for `dev webos-auth-probe`: {}",
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
}

impl fmt::Display for DevError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "could not write webOS auth probe output: {source}"),
            Self::ConfigPath(source) => write!(f, "could not resolve probe config: {source}"),
            Self::Config(source) => write!(f, "could not load probe config: {source}"),
            Self::ConfigOwner(source) => {
                write!(f, "could not resolve probe config owner: {source}")
            }
            Self::TokenStore(source) => {
                write!(f, "could not construct probe access-token store: {source}")
            }
            Self::Authentication(source) => {
                write!(f, "webOS auth probe authentication failed: {source}")
            }
            Self::PowerState(source) => write!(f, "webOS auth probe state read failed: {source}"),
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
        }
    }
}

pub(crate) fn run_dev_command<W: Write>(
    command: DevCommand,
    writer: &mut W,
) -> Result<(), DevError> {
    match command {
        DevCommand::WebOsAuthProbe => run_webos_auth_probe(writer),
    }
}

fn run_webos_auth_probe<W: Write>(writer: &mut W) -> Result<(), DevError> {
    let config_path = resolve_config_path_from_env().map_err(DevError::ConfigPath)?;
    let config = load_config(&config_path).map_err(DevError::Config)?;
    let owner = resolve_config_owner(&config_path).map_err(DevError::ConfigOwner)?;
    let context = WebOsAuthProbeContext::new(&config_path, config.tv_ip, owner)?;

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

struct WebOsAuthProbeContext {
    endpoint: WebOsEndpoint,
    token_store: PlatformAccessTokenStore,
}

impl WebOsAuthProbeContext {
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
    context: &WebOsAuthProbeContext,
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
    let mut progress_error = None;
    let probe_result = {
        let mut on_auth_event = |event| {
            if progress_error.is_none() {
                progress_error =
                    write_auth_event(writer, context.token_store.token_path(), event).err();
            }
        };
        probe(context.endpoint, &context.token_store, &mut on_auth_event)
    };

    if let Some(source) = progress_error {
        return Err(DevError::Io(source));
    }

    let power_state = probe_result?;
    writeln!(
        writer,
        "{WEBOS_AUTH_PROBE_PREFIX}: power_state={power_state}"
    )
    .map_err(DevError::Io)
}

fn write_auth_event<W: Write>(
    writer: &mut W,
    token_path: &Path,
    event: WebOsAuthenticationEvent,
) -> io::Result<()> {
    match event {
        WebOsAuthenticationEvent::UsingStoredAccessToken => {
            writeln!(
                writer,
                "{WEBOS_AUTH_PROBE_PREFIX}: using stored access token."
            )?;
        }
        WebOsAuthenticationEvent::PairingPrompt => {
            writeln!(
                writer,
                "{WEBOS_AUTH_PROBE_PREFIX}: pairing required; accept the prompt on the TV."
            )?;
        }
        WebOsAuthenticationEvent::AccessTokenPersisted => {
            writeln!(
                writer,
                "{WEBOS_AUTH_PROBE_PREFIX}: stored access token at {}",
                token_path.display()
            )?;
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        run_webos_auth_probe_with, DevCommand, DevError, DevParseError, WebOsAuthProbeContext,
    };
    use crate::auth::SystemUser;
    use crate::web_os::{WebOsAuthenticationEvent, WebOsPowerState, WebOsPowerStateError};
    use std::net::Ipv4Addr;
    use std::path::Path;

    fn probe_context() -> WebOsAuthProbeContext {
        WebOsAuthProbeContext::new(
            Path::new("/tmp/lg-buddy-dev-probe/config.env"),
            Ipv4Addr::new(192, 0, 2, 42),
            SystemUser::new("test-user", 1000, 1000, "/tmp"),
        )
        .expect("construct probe context")
    }

    #[test]
    fn parser_accepts_only_webos_auth_probe() {
        assert_eq!(
            DevCommand::parse(["webos-auth-probe"]),
            Ok(DevCommand::WebOsAuthProbe)
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
