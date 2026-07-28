use super::registration::{
    parse_registration_message, WebOsRegistrationError, WebOsRegistrationEvent,
    WebOsRegistrationRequest,
};
use super::tls::webos_tv_self_signed_client_config;
use crate::platform_access_token::{
    PlatformAccessToken, PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore,
};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, Connector, HandshakeError, Message, WebSocket};

const WEBOS_WS_PORT: u16 = 3000;
const WEBOS_WSS_PORT: u16 = 3001;

type WebOsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebOsTransport {
    Ws,
    Wss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsEndpoint {
    address: SocketAddr,
    transport: WebOsTransport,
}

impl WebOsEndpoint {
    pub fn ws(ip: Ipv4Addr) -> Self {
        Self {
            address: SocketAddr::new(IpAddr::V4(ip), WEBOS_WS_PORT),
            transport: WebOsTransport::Ws,
        }
    }

    /// Uses encrypted WSS while accepting the TV's self-signed certificate.
    /// This protects traffic from passive observation but does not establish
    /// the TV's identity through a trusted certificate authority.
    pub fn wss(ip: Ipv4Addr) -> Self {
        Self::wss_at(SocketAddr::new(IpAddr::V4(ip), WEBOS_WSS_PORT))
    }

    #[cfg(test)]
    pub(super) fn ws_at(address: SocketAddr) -> Self {
        Self {
            address,
            transport: WebOsTransport::Ws,
        }
    }

    fn wss_at(address: SocketAddr) -> Self {
        Self {
            address,
            transport: WebOsTransport::Wss,
        }
    }

    fn url(self) -> String {
        let scheme = match self.transport {
            WebOsTransport::Ws => "ws",
            WebOsTransport::Wss => "wss",
        };
        format!("{scheme}://{}/", self.address)
    }
}

impl fmt::Display for WebOsEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url())
    }
}

pub struct WebOsClient {
    socket: WebOsSocket,
    next_request_sequence: u64,
    response_timeout: Duration,
}

impl WebOsClient {
    pub fn connect_authenticated<F>(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        token_store: &PlatformAccessTokenStore,
        mut on_auth_event: F,
    ) -> Result<Self, WebOsAuthenticatedClientError>
    where
        F: FnMut(WebOsAuthenticationEvent),
    {
        let mut client = Self::connect(endpoint, connect_timeout, response_timeout)
            .map_err(|source| WebOsAuthenticatedClientError::Connect { source })?;
        let mut registration = client.registration();
        token_store
            .get_or_acquire(&mut registration, &mut on_auth_event)
            .map_err(|source| WebOsAuthenticatedClientError::Authentication { source })?;

        Ok(client)
    }

    fn connect(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Self, WebOsClientError> {
        if connect_timeout.is_zero() {
            return Err(WebOsClientError::InvalidTimeout { name: "connect" });
        }
        if response_timeout.is_zero() {
            return Err(WebOsClientError::InvalidTimeout { name: "response" });
        }

        let stream = TcpStream::connect_timeout(&endpoint.address, connect_timeout)
            .map_err(|source| WebOsClientError::Connect { endpoint, source })?;
        stream
            .set_nodelay(true)
            .and_then(|_| stream.set_read_timeout(Some(connect_timeout)))
            .and_then(|_| stream.set_write_timeout(Some(connect_timeout)))
            .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

        let connector = match endpoint.transport {
            WebOsTransport::Ws => Connector::Plain,
            WebOsTransport::Wss => Connector::Rustls(webos_tv_self_signed_client_config()),
        };
        let (mut socket, _) =
            match client_tls_with_config(endpoint.url(), stream, None, Some(connector)) {
                Ok(connected) => connected,
                Err(HandshakeError::Failure(source)) => {
                    return Err(WebOsClientError::Handshake { source })
                }
                Err(HandshakeError::Interrupted(_)) => {
                    return Err(WebOsClientError::HandshakeInterrupted)
                }
            };
        set_read_timeout(&mut socket, response_timeout)
            .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

        Ok(Self {
            socket,
            next_request_sequence: 0,
            response_timeout,
        })
    }

    #[cfg(test)]
    pub(super) fn connect_for_test(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Self, WebOsClientError> {
        Self::connect(endpoint, connect_timeout, response_timeout)
    }

    fn registration(&mut self) -> WebOsClientRegistration<'_> {
        WebOsClientRegistration { client: self }
    }

    pub(crate) fn send_request(
        &mut self,
        uri: &str,
        payload: Value,
    ) -> Result<Value, WebOsClientError> {
        let request_id = self.next_request_id()?;
        let request = json!({
            "id": request_id,
            "type": "request",
            "uri": uri,
            "payload": payload,
        });

        self.exchange(&request_id, request)
    }

    fn next_request_id(&mut self) -> Result<String, WebOsClientError> {
        let sequence = self.next_request_sequence;
        self.next_request_sequence = sequence
            .checked_add(1)
            .ok_or(WebOsClientError::RequestIdExhausted)?;
        Ok(format!("request_{sequence}"))
    }

    fn exchange(&mut self, request_id: &str, request: Value) -> Result<Value, WebOsClientError> {
        self.send_message(request)?;
        let deadline = self.response_deadline()?;
        let response = self.receive_correlated(request_id, deadline)?;
        validate_response_message(response)
    }

    fn send_message(&mut self, request: Value) -> Result<(), WebOsClientError> {
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|source| WebOsClientError::Send { source })
    }

    fn response_deadline(&self) -> Result<Instant, WebOsClientError> {
        Instant::now()
            .checked_add(self.response_timeout)
            .ok_or(WebOsClientError::InvalidTimeout { name: "response" })
    }

    fn receive_correlated(
        &mut self,
        request_id: &str,
        deadline: Instant,
    ) -> Result<Value, WebOsClientError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| WebOsClientError::Timeout {
                    request_id: request_id.to_string(),
                })?;
            if remaining.is_zero() {
                return Err(WebOsClientError::Timeout {
                    request_id: request_id.to_string(),
                });
            }
            set_read_timeout(&mut self.socket, remaining)
                .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(source))
                    if matches!(
                        source.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(WebOsClientError::Timeout {
                        request_id: request_id.to_string(),
                    })
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(WebOsClientError::ConnectionClosed {
                        request_id: request_id.to_string(),
                    })
                }
                Err(source) => return Err(WebOsClientError::Receive { source }),
            };

            match message {
                Message::Text(text) => {
                    if let Some(response) = parse_correlated_frame(request_id, text.as_str())? {
                        return Ok(response);
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(WebOsClientError::ConnectionClosed {
                        request_id: request_id.to_string(),
                    })
                }
                Message::Binary(_) => return Err(WebOsClientError::UnexpectedBinaryFrame),
                Message::Frame(_) => return Err(WebOsClientError::UnexpectedRawFrame),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsAuthenticationEvent {
    UsingStoredAccessToken,
    PairingPrompt,
    AccessTokenPersisted,
}

pub(crate) struct WebOsClientRegistration<'client> {
    client: &'client mut WebOsClient,
}

impl WebOsClientRegistration<'_> {
    pub(crate) fn register<F>(
        &mut self,
        access_token: Option<&PlatformAccessToken>,
        on_auth_event: &mut F,
    ) -> Result<PlatformAccessToken, WebOsClientRegistrationError>
    where
        F: FnMut(WebOsAuthenticationEvent),
    {
        let request_id = self
            .client
            .next_request_id()
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
        let request = WebOsRegistrationRequest::new(&request_id, access_token)
            .map_err(|source| WebOsClientRegistrationError::Protocol { source })?;
        self.client
            .send_message(request.to_json_value())
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
        let deadline = self
            .client
            .response_deadline()
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;

        loop {
            let response = self
                .client
                .receive_correlated(&request_id, deadline)
                .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
            let event = parse_registration_message(&request_id, &response.to_string())
                .map_err(|source| WebOsClientRegistrationError::Protocol { source })?;

            match event {
                WebOsRegistrationEvent::PairingPrompt if access_token.is_some() => {
                    return Err(WebOsClientRegistrationError::StoredTokenRequiresPairing)
                }
                WebOsRegistrationEvent::PairingPrompt => {
                    on_auth_event(WebOsAuthenticationEvent::PairingPrompt)
                }
                WebOsRegistrationEvent::Registered { access_token } => return Ok(access_token),
            }
        }
    }
}

#[derive(Debug)]
pub enum WebOsAuthenticatedClientError {
    Connect {
        source: WebOsClientError,
    },
    Authentication {
        source: PlatformAccessTokenAcquisitionError,
    },
}

impl fmt::Display for WebOsAuthenticatedClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { source } => write!(f, "could not connect webOS client: {source}"),
            Self::Authentication { source } => {
                write!(f, "could not authenticate webOS client: {source}")
            }
        }
    }
}

impl Error for WebOsAuthenticatedClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connect { source } => Some(source),
            Self::Authentication { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum WebOsClientRegistrationError {
    Transport { source: WebOsClientError },
    Protocol { source: WebOsRegistrationError },
    StoredTokenRequiresPairing,
}

impl fmt::Display for WebOsClientRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { source } => {
                write!(f, "webOS registration transport failed: {source}")
            }
            Self::Protocol { source } => write!(f, "webOS registration failed: {source}"),
            Self::StoredTokenRequiresPairing => {
                write!(
                    f,
                    "webOS rejected the stored access token and requested pairing"
                )
            }
        }
    }
}

impl Error for WebOsClientRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport { source } => Some(source),
            Self::Protocol { source } => Some(source),
            Self::StoredTokenRequiresPairing => None,
        }
    }
}

#[derive(Debug)]
pub enum WebOsClientError {
    InvalidTimeout {
        name: &'static str,
    },
    Connect {
        endpoint: WebOsEndpoint,
        source: io::Error,
    },
    ConfigureSocket {
        source: io::Error,
    },
    Handshake {
        source: tungstenite::Error,
    },
    HandshakeInterrupted,
    RequestIdExhausted,
    Send {
        source: tungstenite::Error,
    },
    Timeout {
        request_id: String,
    },
    ConnectionClosed {
        request_id: String,
    },
    Receive {
        source: tungstenite::Error,
    },
    MalformedJson {
        source: serde_json::Error,
    },
    InvalidFrameRoot,
    MissingResponseId,
    InvalidResponseId,
    MissingMessageType,
    InvalidMessageType,
    MissingWebOsErrorMessage,
    InvalidWebOsErrorMessage,
    WebOs {
        code: Option<i32>,
        message: String,
    },
    UnexpectedBinaryFrame,
    UnexpectedRawFrame,
}

impl fmt::Display for WebOsClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimeout { name } => {
                write!(f, "webOS {name} timeout must be greater than zero")
            }
            Self::Connect { endpoint, source } => {
                write!(
                    f,
                    "could not connect to webOS endpoint `{endpoint}`: {source}"
                )
            }
            Self::ConfigureSocket { source } => {
                write!(f, "could not configure webOS socket: {source}")
            }
            Self::Handshake { source } => write!(f, "webOS websocket handshake failed: {source}"),
            Self::HandshakeInterrupted => {
                write!(f, "webOS websocket handshake was interrupted")
            }
            Self::RequestIdExhausted => write!(f, "webOS request ID sequence is exhausted"),
            Self::Send { source } => write!(f, "could not send webOS request: {source}"),
            Self::Timeout { request_id } => {
                write!(f, "timed out waiting for webOS response `{request_id}`")
            }
            Self::ConnectionClosed { request_id } => write!(
                f,
                "webOS connection closed before response `{request_id}` arrived"
            ),
            Self::Receive { source } => write!(f, "could not receive webOS response: {source}"),
            Self::MalformedJson { source } => {
                write!(f, "webOS response is malformed JSON: {source}")
            }
            Self::InvalidFrameRoot => write!(f, "webOS response root is not an object"),
            Self::MissingResponseId => write!(f, "webOS response has no request ID"),
            Self::InvalidResponseId => write!(f, "webOS response request ID is not a string"),
            Self::MissingMessageType => write!(f, "webOS response has no message type"),
            Self::InvalidMessageType => write!(f, "webOS response message type is not a string"),
            Self::MissingWebOsErrorMessage => {
                write!(f, "webOS error response has no error message")
            }
            Self::InvalidWebOsErrorMessage => {
                write!(f, "webOS error response message is not a string")
            }
            Self::WebOs {
                code: Some(code),
                message,
            } => write!(f, "webOS error {code}: {message}"),
            Self::WebOs {
                code: None,
                message,
            } => write!(f, "webOS error: {message}"),
            Self::UnexpectedBinaryFrame => write!(f, "webOS sent an unexpected binary frame"),
            Self::UnexpectedRawFrame => write!(f, "webOS sent an unexpected raw frame"),
        }
    }
}

impl Error for WebOsClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connect { source, .. } | Self::ConfigureSocket { source } => Some(source),
            Self::Handshake { source } | Self::Send { source } | Self::Receive { source } => {
                Some(source)
            }
            Self::MalformedJson { source } => Some(source),
            Self::InvalidTimeout { .. }
            | Self::HandshakeInterrupted
            | Self::RequestIdExhausted
            | Self::Timeout { .. }
            | Self::ConnectionClosed { .. }
            | Self::InvalidFrameRoot
            | Self::MissingResponseId
            | Self::InvalidResponseId
            | Self::MissingMessageType
            | Self::InvalidMessageType
            | Self::MissingWebOsErrorMessage
            | Self::InvalidWebOsErrorMessage
            | Self::WebOs { .. }
            | Self::UnexpectedBinaryFrame
            | Self::UnexpectedRawFrame => None,
        }
    }
}

fn set_read_timeout(socket: &mut WebOsSocket, timeout: Duration) -> io::Result<()> {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported webOS TLS stream",
            ))
        }
    };
    stream.set_read_timeout(Some(timeout))
}

fn parse_correlated_frame(
    expected_request_id: &str,
    raw_message: &str,
) -> Result<Option<Value>, WebOsClientError> {
    let message: Value = serde_json::from_str(raw_message)
        .map_err(|source| WebOsClientError::MalformedJson { source })?;
    let object = message
        .as_object()
        .ok_or(WebOsClientError::InvalidFrameRoot)?;
    let actual_request_id = match object.get("id") {
        Some(Value::String(request_id)) => request_id,
        Some(_) => return Err(WebOsClientError::InvalidResponseId),
        None => return Err(WebOsClientError::MissingResponseId),
    };
    if actual_request_id != expected_request_id {
        return Ok(None);
    }

    Ok(Some(message))
}

fn validate_response_message(message: Value) -> Result<Value, WebOsClientError> {
    let object = message
        .as_object()
        .expect("correlated websocket message root was already validated");
    let message_type = match object.get("type") {
        Some(Value::String(message_type)) => message_type,
        Some(_) => return Err(WebOsClientError::InvalidMessageType),
        None => return Err(WebOsClientError::MissingMessageType),
    };
    if message_type == "error" {
        return Err(parse_webos_error(object));
    }

    Ok(message)
}

fn parse_webos_error(message: &serde_json::Map<String, Value>) -> WebOsClientError {
    let error = match message.get("error") {
        Some(Value::String(error)) => error,
        Some(_) => return WebOsClientError::InvalidWebOsErrorMessage,
        None => return WebOsClientError::MissingWebOsErrorMessage,
    };
    let mut parts = error.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or_default();
    let (code, message) = match first.parse::<i32>() {
        Ok(code) => (
            Some(code),
            parts.next().unwrap_or_default().trim().to_string(),
        ),
        Err(_) => (None, error.to_string()),
    };
    WebOsClientError::WebOs { code, message }
}

#[cfg(test)]
mod tests {
    use super::{
        WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsClientError,
        WebOsClientRegistrationError, WebOsEndpoint,
    };
    use crate::auth::SystemUser;
    use crate::platform_access_token::{
        PlatformAccessToken, PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore,
        PlatformAccessTokenStoreError, PlatformAccessTokenStoreOperation,
    };
    use crate::web_os::test_support::{ScriptedWebOsPeer, ScriptedWebOsServer};
    use crate::web_os::{WebOsPowerState, WebOsPowerStateError, WebOsRegistrationError};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rustls::crypto::ring;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};
    use serde_json::{json, Value};
    use std::fs;
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use tungstenite::accept;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RESPONSE_TIMEOUT: Duration = Duration::from_millis(200);
    const TEST_CERTIFICATE_DER: &str = "MIIBkzCCATmgAwIBAgIUGCxGaL477t4FECFewoE+24e3aWQwCgYIKoZIzj0EAwIwHjEcMBoGA1UEAwwTTEcgQnVkZHkgRDMgVGVzdCBUVjAgFw0yNjA3MTEyMDUyMzBaGA8yMTI2MDYxNzIwNTIzMFowHjEcMBoGA1UEAwwTTEcgQnVkZHkgRDMgVGVzdCBUVjBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABE8Q+pVxjrGZr5oQfOCxyA7rl7PVzI9U7ukGEp3PI7r8dWDlmcrT5GdNVXIhjza7yi9YRRjw66NaWAlQsd1zxZWjUzBRMB0GA1UdDgQWBBQmbnb3C0PKQVuvqb8oT0Ur6tZLkzAfBgNVHSMEGDAWgBQmbnb3C0PKQVuvqb8oT0Ur6tZLkzAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0gAMEUCIQDTL/fP5PjETr2XgdN9cBuSkMR8DD0ohRjvya0dfXhM4gIgKe13t3ClUgKULtYbtIa3mwcSCwSsAEfoRsZG5zFiCc8=";
    const TEST_PRIVATE_KEY_DER: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgRQOBcIAKtXbi9IkKmq6PBMKSMlLega0uR6twK6hSmYmhRANCAARPEPqVcY6xma+aEHzgscgO65ez1cyPVO7pBhKdzyO6/HVg5ZnK0+RnTVVyIY82u8ovWEUY8OujWlgJULHdc8WV";
    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-web-os-client-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn token(value: &str) -> PlatformAccessToken {
        PlatformAccessToken::new(value).expect("valid platform access token")
    }

    fn token_store(dir: &TestDir) -> PlatformAccessTokenStore {
        #[cfg(unix)]
        let owner = SystemUser::new(
            "test-user",
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
            dir.path(),
        );
        #[cfg(not(unix))]
        let owner = SystemUser::new("test-user", 0, 0, dir.path());

        PlatformAccessTokenStore::for_primary_profile(&dir.path().join("config.env"), owner)
            .expect("derive test token store")
    }

    struct ScriptedTlsServer {
        endpoint: WebOsEndpoint,
        handle: JoinHandle<()>,
    }

    impl ScriptedTlsServer {
        fn endpoint(&self) -> WebOsEndpoint {
            self.endpoint
        }

        fn finish(self) {
            self.handle.join().expect("scripted TLS server thread");
        }
    }

    fn spawn_tls_server<F>(script: F) -> ScriptedTlsServer
    where
        F: FnOnce(&mut ScriptedWebOsPeer<StreamOwned<ServerConnection, TcpStream>>)
            + Send
            + 'static,
    {
        let certificate = CertificateDer::from(
            STANDARD
                .decode(TEST_CERTIFICATE_DER)
                .expect("decode test certificate"),
        );
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            STANDARD
                .decode(TEST_PRIVATE_KEY_DER)
                .expect("decode test private key"),
        ));
        let provider = Arc::new(ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .expect("build test TLS config");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted TLS server");
        let endpoint = WebOsEndpoint::wss_at(listener.local_addr().expect("server address"));
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept TLS client");
            let connection =
                ServerConnection::new(Arc::new(config)).expect("create TLS connection");
            let stream = StreamOwned::new(connection, stream);
            let socket = accept(stream).expect("accept secure websocket");
            let mut peer = ScriptedWebOsPeer::from_socket(socket);
            script(&mut peer);
        });
        ScriptedTlsServer { endpoint, handle }
    }

    #[test]
    fn requests_use_stable_ids_and_return_the_matching_response() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            for sequence in 0..2 {
                let request = peer.receive_json();
                let request_id = format!("request_{sequence}");
                assert_eq!(request["id"], request_id);
                assert_eq!(request["type"], "request");
                assert_eq!(request["uri"], format!("ssap://test/{sequence}"));
                peer.send_json(json!({
                    "id": request_id,
                    "type": "response",
                    "payload": {"sequence": sequence},
                }));
            }
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        for sequence in 0..2 {
            let response = client
                .send_request(&format!("ssap://test/{sequence}"), json!({}))
                .expect("matching response");
            assert_eq!(response["payload"]["sequence"], sequence);
        }
        server.finish();
    }

    #[test]
    fn unrelated_frame_is_ignored_before_matching_response() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            let request_id = request["id"].as_str().expect("request ID");
            peer.send_json(json!({"id": "subscription_0", "type": "response", "payload": {}}));
            peer.send_json(json!({"id": request_id, "type": "response", "payload": {"ok": true}}));
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        let response = client
            .send_request("ssap://test/correlated", json!({}))
            .expect("matching response");
        assert_eq!(response["payload"]["ok"], true);
        server.finish();
    }

    #[test]
    fn wrong_response_id_is_not_accepted_and_deadline_is_absolute() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let _request = peer.receive_json();
            peer.send_json(json!({"id": "wrong", "type": "response", "payload": {"ok": true}}));
            thread::sleep(Duration::from_millis(150));
        });
        let mut client = WebOsClient::connect(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_millis(40),
        )
        .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/wrong-id", json!({})),
            Err(WebOsClientError::Timeout { request_id }) if request_id == "request_0"
        ));
        server.finish();
    }

    #[test]
    fn close_before_matching_response_is_typed() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let _request = peer.receive_json();
            peer.send_close();
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/close", json!({})),
            Err(WebOsClientError::ConnectionClosed { request_id })
                if request_id == "request_0"
        ));
        server.finish();
    }

    #[test]
    fn malformed_text_frame_is_typed() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let _request = peer.receive_json();
            peer.send_text("not-json");
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/malformed", json!({})),
            Err(WebOsClientError::MalformedJson { .. })
        ));
        server.finish();
    }

    #[test]
    fn webos_error_preserves_code_and_message() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "error",
                "error": "-401 not permitted",
            }));
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/error", json!({})),
            Err(WebOsClientError::WebOs {
                code: Some(-401),
                message,
            }) if message == "not permitted"
        ));
        server.finish();
    }

    #[test]
    fn connection_failure_is_typed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve address");
        let endpoint = WebOsEndpoint::ws_at(listener.local_addr().expect("reserved address"));
        drop(listener);

        assert!(matches!(
            WebOsClient::connect(endpoint, CONNECT_TIMEOUT, RESPONSE_TIMEOUT),
            Err(WebOsClientError::Connect {
                endpoint: failed_endpoint,
                ..
            }) if failed_endpoint == endpoint
        ));
    }

    #[test]
    fn wss_accepts_self_signed_tv_certificate() {
        let server = spawn_tls_server(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"encrypted": true},
            }));
        });
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect secure client");

        let response = client
            .send_request("ssap://test/wss", json!({}))
            .expect("secure response");
        assert_eq!(response["payload"]["encrypted"], true);
        server.finish();
    }

    #[test]
    fn authenticated_client_registers_with_stored_token_without_prompt_or_rewrite() {
        let dir = TestDir::new("stored-token");
        let store = token_store(&dir);
        let original = token("stored-client-key");
        store.persist(&original).expect("persist stored token");
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            assert_eq!(request["type"], "register");
            assert_eq!(request["payload"]["client-key"], "stored-client-key");
            peer.send_json(json!({
                "id": request["id"],
                "type": "registered",
                "payload": {"client-key": "replacement-client-key"},
            }));
        });
        let mut events = Vec::new();

        let _client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        )
        .expect("authenticate with stored token");

        assert_eq!(
            events,
            vec![WebOsAuthenticationEvent::UsingStoredAccessToken]
        );
        assert_eq!(store.load().expect("reload stored token"), Some(original));
        server.finish();
    }

    #[test]
    fn authenticated_client_pairs_and_persists_new_token() {
        let dir = TestDir::new("new-token");
        let store = token_store(&dir);
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            assert_eq!(request["type"], "register");
            assert_eq!(request["payload"]["client-key"], Value::Null);
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"pairingType": "PROMPT", "returnValue": true},
            }));
            peer.send_json(json!({
                "id": request["id"],
                "type": "registered",
                "payload": {"client-key": "new-client-key"},
            }));
        });
        let mut events = Vec::new();

        let _client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        )
        .expect("pair and authenticate client");

        assert_eq!(
            events,
            vec![
                WebOsAuthenticationEvent::PairingPrompt,
                WebOsAuthenticationEvent::AccessTokenPersisted,
            ]
        );
        assert_eq!(
            store.load().expect("load acquired token"),
            Some(token("new-client-key"))
        );
        server.finish();
    }

    #[test]
    fn stored_token_pairing_request_is_typed_and_preserves_token() {
        let dir = TestDir::new("rejected-stored-token");
        let store = token_store(&dir);
        let original = token("rejected-client-key");
        store.persist(&original).expect("persist stored token");
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"pairingType": "PROMPT", "returnValue": true},
            }));
        });
        let mut events = Vec::new();

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::StoredTokenRequiresPairing,
                },
            })
        ));
        assert_eq!(
            events,
            vec![WebOsAuthenticationEvent::UsingStoredAccessToken]
        );
        assert_eq!(store.load().expect("reload stored token"), Some(original));
        server.finish();
    }

    #[test]
    fn pairing_rejection_does_not_create_token() {
        let dir = TestDir::new("pairing-rejected");
        let store = token_store(&dir);
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"returnValue": false, "errorText": "pairing denied"},
            }));
        });

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Protocol {
                        source: WebOsRegistrationError::PairingRejected {
                            message: Some(message),
                        },
                    },
                },
            }) if message == "pairing denied"
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn registration_timeout_does_not_create_token() {
        let dir = TestDir::new("registration-timeout");
        let store = token_store(&dir);
        let server = ScriptedWebOsServer::spawn(|peer| {
            let _request = peer.receive_json();
            thread::sleep(Duration::from_millis(150));
        });

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_millis(40),
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Transport {
                        source: WebOsClientError::Timeout { .. },
                    },
                },
            })
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn malformed_registration_does_not_create_token() {
        let dir = TestDir::new("malformed-registration");
        let store = token_store(&dir);
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "registered",
                "payload": {},
            }));
        });

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Protocol {
                        source: WebOsRegistrationError::MissingClientKey,
                    },
                },
            })
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn persistence_failure_does_not_return_authenticated_client() {
        let dir = TestDir::new("persistence-failure");
        let store = token_store(&dir);
        let token_path = store.token_path().to_path_buf();
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"pairingType": "PROMPT", "returnValue": true},
            }));
            peer.send_json(json!({
                "id": request["id"],
                "type": "registered",
                "payload": {"client-key": "unpersisted-client-key"},
            }));
        });

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| {
                if event == WebOsAuthenticationEvent::PairingPrompt {
                    fs::create_dir_all(&token_path).expect("block token file replacement");
                }
            },
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Store {
                    source: PlatformAccessTokenStoreError::Io {
                        operation: PlatformAccessTokenStoreOperation::ReplaceToken,
                        ..
                    },
                },
            })
        ));
        assert!(store.token_path().is_dir());
        server.finish();
    }

    #[test]
    fn authenticated_client_reads_typed_power_state() {
        let dir = TestDir::new("power-state");
        let store = token_store(&dir);
        store
            .persist(&token("power-state-client-key"))
            .expect("persist stored token");
        let server = ScriptedWebOsServer::spawn(|peer| {
            let registration = peer.receive_json();
            assert_eq!(registration["type"], "register");
            assert_eq!(
                registration["payload"]["client-key"],
                "power-state-client-key"
            );
            peer.send_json(json!({
                "id": registration["id"],
                "type": "registered",
                "payload": {"client-key": "power-state-client-key"},
            }));

            let request = peer.receive_json();
            assert_eq!(request["id"], "request_1");
            assert_eq!(request["type"], "request");
            assert_eq!(
                request["uri"],
                "ssap://com.webos.service.tvpower/power/getPowerState"
            );
            assert_eq!(request["payload"], json!({}));
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {"returnValue": true, "state": "Active"},
            }));
        });
        let mut client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| assert_eq!(event, WebOsAuthenticationEvent::UsingStoredAccessToken),
        )
        .expect("authenticate power-state client");

        assert_eq!(
            client.power_state().expect("read power state"),
            WebOsPowerState::Active
        );
        server.finish();
    }

    #[test]
    fn power_state_preserves_webos_error() {
        let dir = TestDir::new("power-state-error");
        let store = token_store(&dir);
        store
            .persist(&token("power-state-error-key"))
            .expect("persist stored token");
        let server = ScriptedWebOsServer::spawn(|peer| {
            let registration = peer.receive_json();
            peer.send_json(json!({
                "id": registration["id"],
                "type": "registered",
                "payload": {"client-key": "power-state-error-key"},
            }));

            let request = peer.receive_json();
            peer.send_json(json!({
                "id": request["id"],
                "type": "error",
                "error": "-401 not permitted",
            }));
        });
        let mut client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| assert_eq!(event, WebOsAuthenticationEvent::UsingStoredAccessToken),
        )
        .expect("authenticate power-state client");

        assert!(matches!(
            client.power_state(),
            Err(WebOsPowerStateError::Request {
                source: WebOsClientError::WebOs {
                    code: Some(-401),
                    message,
                },
            }) if message == "not permitted"
        ));
        server.finish();
    }
}
