//! Central stateful webOS test server and semantic fault scenarios.

use super::super::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsEndpoint,
    WebOsPowerState,
};
use crate::auth::SystemUser;
use crate::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tungstenite::error::ProtocolError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::{accept, Error as WebSocketError, Message, WebSocket};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);
const TEST_ACCESS_TOKEN: &str = "webos-test-access-token";
const TEST_REPLACEMENT_ACCESS_TOKEN: &str = "replacement-client-key";
const TEST_CERTIFICATE_DER: &str = "MIIBkzCCATmgAwIBAgIUGCxGaL477t4FECFewoE+24e3aWQwCgYIKoZIzj0EAwIwHjEcMBoGA1UEAwwTTEcgQnVkZHkgRDMgVGVzdCBUVjAgFw0yNjA3MTEyMDUyMzBaGA8yMTI2MDYxNzIwNTIzMFowHjEcMBoGA1UEAwwTTEcgQnVkZHkgRDMgVGVzdCBUVjBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABE8Q+pVxjrGZr5oQfOCxyA7rl7PVzI9U7ukGEp3PI7r8dWDlmcrT5GdNVXIhjza7yi9YRRjw66NaWAlQsd1zxZWjUzBRMB0GA1UdDgQWBBQmbnb3C0PKQVuvqb8oT0Ur6tZLkzAfBgNVHSMEGDAWgBQmbnb3C0PKQVuvqb8oT0Ur6tZLkzAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0gAMEUCIQDTL/fP5PjETr2XgdN9cBuSkMR8DD0ohRjvya0dfXhM4gIgKe13t3ClUgKULtYbtIa3mwcSCwSsAEfoRsZG5zFiCc8=";
const TEST_PRIVATE_KEY_DER: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgRQOBcIAKtXbi9IkKmq6PBMKSMlLega0uR6twK6hSmYmhRANCAARPEPqVcY6xma+aEHzgscgO65ez1cyPVO7pBhKdzyO6/HVg5ZnK0+RnTVVyIY82u8ovWEUY8OujWlgJULHdc8WV";

const GET_FOREGROUND_APP_URI: &str = "ssap://com.webos.applicationManager/getForegroundAppInfo";
const GET_POWER_STATE_URI: &str = "ssap://com.webos.service.tvpower/power/getPowerState";
const GET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/getSystemSettings";
const SET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/setSystemSettings";
const SET_INPUT_URI: &str = "ssap://tv/switchInput";
const TURN_OFF_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";
const TURN_ON_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";
const TURN_OFF_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOffScreen";
const TURN_ON_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOnScreen";
const POWER_OFF_URI: &str = "ssap://system/turnOff";

static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static WRITE_SETTINGS_SIGNED_ENVELOPE: OnceLock<Value> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::web_os) enum WebOsTestScenario {
    StatefulTv,
    ProtocolEcho,
    UnrelatedFrameBeforeResponse,
    WrongResponseId,
    CloseBeforeResponse,
    MalformedTextFrame,
    WebOsError,
    StoredTokenReplacement,
    StoredTokenPairingPrompt,
    PairingRejected,
    RegistrationTimeout,
    RegistrationMissingClientKey,
    PowerStatePermissionDenied,
    BacklightWriteAcknowledgedWithoutChange,
    CloseAfterFirstInputWrite,
    StallFirstRequest,
    SameInputWriteAcknowledgedWhileScreenOff,
    RestoreSessionInterruptedAndInputAckLeavesScreenOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebOsTestTransport {
    Plain,
    Tls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::web_os) enum WebOsTestInput {
    Hdmi2,
    Hdmi3,
}

impl WebOsTestInput {
    fn app_id(self) -> &'static str {
        match self {
            Self::Hdmi2 => "com.webos.app.hdmi2",
            Self::Hdmi3 => "com.webos.app.hdmi3",
        }
    }

    fn from_input_id(value: &str) -> Self {
        match value {
            "HDMI_2" => Self::Hdmi2,
            "HDMI_3" => Self::Hdmi3,
            other => panic!("no real-TV observation exists for input `{other}`"),
        }
    }

    fn backlight(self) -> Value {
        match self {
            Self::Hdmi2 => json!("90"),
            Self::Hdmi3 => json!(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::web_os) struct WebOsTestTvSnapshot {
    pub(in crate::web_os) power_state: WebOsPowerState,
    pub(in crate::web_os) input: WebOsTestInput,
    pub(in crate::web_os) backlight: Value,
    pub(in crate::web_os) connection_count: u64,
    pub(in crate::web_os) pairing_prompt_count: u64,
    pub(in crate::web_os) registration_tokens: Vec<Option<String>>,
}

struct WebOsTestTv {
    power_state: WebOsPowerState,
    input: WebOsTestInput,
    backlight: Value,
}

#[derive(Default)]
struct WebOsTestPermissions {
    top_level: HashSet<String>,
    write_settings_authorized: bool,
}

impl WebOsTestTv {
    fn new(power_state: WebOsPowerState, input: WebOsTestInput) -> Self {
        Self {
            power_state,
            input,
            backlight: input.backlight(),
        }
    }

    fn handle_request(
        &mut self,
        request: &Value,
        permissions: &WebOsTestPermissions,
        apply_backlight_write: bool,
        acknowledge_same_input_while_screen_off: bool,
    ) -> Value {
        assert_eq!(request["type"], "request", "expected webOS request frame");
        let request_id = request["id"].as_str().expect("webOS request ID");
        let uri = request["uri"].as_str().expect("webOS request URI");
        let payload = request
            .get("payload")
            .expect("webOS request payload must be present");

        match uri {
            GET_POWER_STATE_URI => {
                require_top_level_permission(permissions, "READ_POWER_STATE", uri);
                assert_eq!(payload, &json!({}));
                assert_ne!(self.power_state, WebOsPowerState::PowerOff);
                response(
                    request_id,
                    json!({
                        "returnValue": true,
                        "state": self.power_state.to_string(),
                    }),
                )
            }
            GET_FOREGROUND_APP_URI => {
                if !permissions.top_level.contains("READ_RUNNING_APPS") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(payload, &json!({}));
                assert_eq!(self.power_state, WebOsPowerState::Active);
                response(
                    request_id,
                    json!({
                        "appId": self.input.app_id(),
                        "processId": "",
                        "returnValue": true,
                        "windowId": "",
                    }),
                )
            }
            GET_SYSTEM_SETTINGS_URI => {
                if !permissions.top_level.contains("READ_SETTINGS") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(
                    payload,
                    &json!({
                        "category": "picture",
                        "keys": ["backlight"],
                    })
                );
                assert_eq!(self.power_state, WebOsPowerState::Active);
                response(
                    request_id,
                    json!({
                        "category": "picture",
                        "returnValue": true,
                        "settings": {"backlight": self.backlight},
                        "subscribed": false,
                    }),
                )
            }
            SET_SYSTEM_SETTINGS_URI => {
                if !permissions.write_settings_authorized {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(self.power_state, WebOsPowerState::Active);
                let backlight = payload["settings"]["backlight"]
                    .as_u64()
                    .expect("set-system-settings backlight value");
                assert!(backlight <= 100, "backlight must be between 0 and 100");
                assert_eq!(
                    payload,
                    &json!({
                        "category": "picture",
                        "settings": {"backlight": backlight},
                    })
                );
                if apply_backlight_write {
                    self.backlight = json!(backlight);
                }
                response(
                    request_id,
                    json!({"method": "setSystemSettings", "returnValue": true}),
                )
            }
            SET_INPUT_URI => {
                if !permissions.top_level.contains("LAUNCH") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                let input_id = payload["inputId"]
                    .as_str()
                    .expect("switch-input request input ID");
                assert_eq!(payload, &json!({"inputId": input_id}));
                let requested_input = WebOsTestInput::from_input_id(input_id);
                if self.power_state == WebOsPowerState::ScreenOff {
                    assert!(
                        acknowledge_same_input_while_screen_off,
                        "no real-TV observation exists for switching input while Screen Off"
                    );
                    assert_eq!(
                        requested_input, self.input,
                        "the real-TV Screen Off observation only covers acknowledging the current input"
                    );
                    return response(request_id, json!({"returnValue": true}));
                }
                assert_eq!(self.power_state, WebOsPowerState::Active);
                self.input = requested_input;
                self.backlight = self.input.backlight();
                response(request_id, json!({"returnValue": true}))
            }
            TURN_OFF_SCREEN_URI => {
                if !permissions.top_level.contains("CONTROL_TV_SCREEN") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(payload, &json!({"standbyMode": "active"}));
                assert_eq!(self.power_state, WebOsPowerState::Active);
                self.power_state = WebOsPowerState::ScreenOff;
                response(
                    request_id,
                    json!({"returnValue": true, "state": "Screen Off"}),
                )
            }
            TURN_ON_SCREEN_URI => {
                if !permissions.top_level.contains("CONTROL_TV_SCREEN") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(payload, &json!({"standbyMode": "active"}));
                match &self.power_state {
                    WebOsPowerState::ScreenOff => {
                        self.power_state = WebOsPowerState::Active;
                        response(request_id, json!({"returnValue": true, "state": "Active"}))
                    }
                    WebOsPowerState::Active => webos_error(
                        request_id,
                        "500 Application error",
                        json!({
                            "errorCode": "-102",
                            "errorText": "The current sub state must be 'screen off'",
                            "returnValue": false,
                            "state": "Active",
                        }),
                    ),
                    state => {
                        panic!("no real-TV screen-on observation exists for power state `{state}`")
                    }
                }
            }
            TURN_OFF_SCREEN_LEGACY_URI | TURN_ON_SCREEN_LEGACY_URI => {
                assert_eq!(payload, &json!({"standbyMode": "active"}));
                webos_error(request_id, "404 no such service or method", json!({}))
            }
            POWER_OFF_URI => {
                require_top_level_permission(permissions, "CONTROL_POWER", uri);
                assert_eq!(payload, &json!({}));
                assert_eq!(self.power_state, WebOsPowerState::Active);
                self.power_state = WebOsPowerState::PowerOff;
                response(request_id, json!({"returnValue": true}))
            }
            other => panic!("no real-TV observation exists for webOS URI `{other}`"),
        }
    }
}

struct WebOsTestRuntime {
    tv: WebOsTestTv,
    scenario: WebOsTestScenario,
    connection_count: u64,
    pairing_prompt_count: u64,
    registration_tokens: Vec<Option<String>>,
    ambiguous_input_write_injected: bool,
    stalled_request_injected: bool,
    restore_session_interruption_injected: bool,
}

pub(in crate::web_os) struct WebOsTestServer {
    endpoint: WebOsEndpoint,
    address: std::net::SocketAddr,
    runtime: Arc<Mutex<WebOsTestRuntime>>,
    active_connection: Arc<Mutex<Option<TcpStream>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WebOsTestServer {
    pub(in crate::web_os) fn active(input: WebOsTestInput) -> Self {
        Self::spawn(
            WebOsPowerState::Active,
            input,
            WebOsTestScenario::StatefulTv,
            WebOsTestTransport::Plain,
        )
    }

    pub(in crate::web_os) fn screen_off(input: WebOsTestInput) -> Self {
        Self::spawn(
            WebOsPowerState::ScreenOff,
            input,
            WebOsTestScenario::StatefulTv,
            WebOsTestTransport::Plain,
        )
    }

    pub(in crate::web_os) fn for_scenario(scenario: WebOsTestScenario) -> Self {
        Self::spawn(
            WebOsPowerState::Active,
            WebOsTestInput::Hdmi3,
            scenario,
            WebOsTestTransport::Plain,
        )
    }

    pub(in crate::web_os) fn for_tls_scenario(scenario: WebOsTestScenario) -> Self {
        Self::spawn(
            WebOsPowerState::Active,
            WebOsTestInput::Hdmi3,
            scenario,
            WebOsTestTransport::Tls,
        )
    }

    #[allow(dead_code)]
    pub(in crate::web_os) fn active_tls_at(
        input: WebOsTestInput,
        address: std::net::SocketAddr,
    ) -> Self {
        Self::spawn_at(
            WebOsPowerState::Active,
            input,
            WebOsTestScenario::StatefulTv,
            WebOsTestTransport::Tls,
            address,
        )
    }

    fn spawn(
        power_state: WebOsPowerState,
        input: WebOsTestInput,
        scenario: WebOsTestScenario,
        transport: WebOsTestTransport,
    ) -> Self {
        Self::spawn_at(
            power_state,
            input,
            scenario,
            transport,
            "127.0.0.1:0".parse().expect("webOS test bind address"),
        )
    }

    fn spawn_at(
        power_state: WebOsPowerState,
        input: WebOsTestInput,
        scenario: WebOsTestScenario,
        transport: WebOsTestTransport,
        bind_address: std::net::SocketAddr,
    ) -> Self {
        let listener = TcpListener::bind(bind_address)
            .unwrap_or_else(|error| panic!("bind webOS test server at {bind_address}: {error}"));
        listener
            .set_nonblocking(true)
            .expect("configure webOS test listener");
        let address = listener.local_addr().expect("webOS test address");
        let endpoint = match transport {
            WebOsTestTransport::Plain => WebOsEndpoint::ws_at(address),
            WebOsTestTransport::Tls => WebOsEndpoint::wss_at(address),
        };
        let runtime = Arc::new(Mutex::new(WebOsTestRuntime {
            tv: WebOsTestTv::new(power_state, input),
            scenario,
            connection_count: 0,
            pairing_prompt_count: 0,
            registration_tokens: Vec::new(),
            ambiguous_input_write_injected: false,
            stalled_request_injected: false,
            restore_session_interruption_injected: false,
        }));
        let active_connection = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let server_runtime = Arc::clone(&runtime);
        let server_active_connection = Arc::clone(&active_connection);
        let server_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !server_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if server_stop.load(Ordering::Acquire) {
                            break;
                        }
                        *server_active_connection
                            .lock()
                            .expect("webOS test active connection") = Some(
                            stream
                                .try_clone()
                                .expect("clone webOS test connection for shutdown"),
                        );
                        serve_accepted_connection(stream, transport, &server_runtime, &server_stop);
                        *server_active_connection
                            .lock()
                            .expect("webOS test active connection") = None;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept webOS test connection: {error}"),
                }
            }
        });

        Self {
            endpoint,
            address,
            runtime,
            active_connection,
            stop,
            handle: Some(handle),
        }
    }

    pub(in crate::web_os) fn connect_authenticated(
        &self,
    ) -> Result<WebOsClient, WebOsAuthenticatedClientError> {
        let token_fixture = TestAccessTokenStore::new();
        WebOsClient::connect_authenticated(
            self.endpoint,
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            token_fixture.store(),
            |event| assert_eq!(event, WebOsAuthenticationEvent::UsingStoredAccessToken),
        )
    }

    pub(in crate::web_os) fn endpoint(&self) -> WebOsEndpoint {
        self.endpoint
    }

    pub(in crate::web_os) fn access_token(&self) -> PlatformAccessToken {
        PlatformAccessToken::new(TEST_ACCESS_TOKEN).expect("webOS test access token")
    }

    #[allow(dead_code)]
    pub(in crate::web_os) fn set_scenario(&self, scenario: WebOsTestScenario) {
        self.runtime
            .lock()
            .expect("webOS test server state")
            .scenario = scenario;
    }

    #[allow(dead_code)]
    pub(in crate::web_os) fn assert_healthy(&self) {
        assert!(
            !self.handle.as_ref().is_some_and(JoinHandle::is_finished),
            "webOS test server stopped unexpectedly"
        );
    }

    pub(in crate::web_os) fn snapshot(&self) -> WebOsTestTvSnapshot {
        let runtime = self.runtime.lock().expect("webOS test TV state");
        WebOsTestTvSnapshot {
            power_state: runtime.tv.power_state.clone(),
            input: runtime.tv.input,
            backlight: runtime.tv.backlight.clone(),
            connection_count: runtime.connection_count,
            pairing_prompt_count: runtime.pairing_prompt_count,
            registration_tokens: runtime.registration_tokens.clone(),
        }
    }

    pub(in crate::web_os) fn finish(mut self) {
        self.stop_and_join().expect("webOS test server thread");
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Release);
        if let Some(connection) = self
            .active_connection
            .lock()
            .expect("webOS test active connection")
            .as_ref()
        {
            let _ = connection.shutdown(Shutdown::Both);
        }
        let _ = TcpStream::connect(self.address);
        handle.join()
    }
}

impl Drop for WebOsTestServer {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn serve_accepted_connection(
    stream: TcpStream,
    transport: WebOsTestTransport,
    runtime: &Arc<Mutex<WebOsTestRuntime>>,
    stop: &AtomicBool,
) {
    match transport {
        WebOsTestTransport::Plain => {
            let socket = accept(stream).expect("accept webOS test websocket");
            serve_connection(socket, runtime, stop);
        }
        WebOsTestTransport::Tls => {
            let connection =
                ServerConnection::new(test_tls_config()).expect("create webOS test TLS connection");
            let stream = StreamOwned::new(connection, stream);
            let socket = accept(stream).expect("accept secure webOS test websocket");
            serve_connection(socket, runtime, stop);
        }
    }
}

fn serve_connection<S>(
    mut socket: WebSocket<S>,
    runtime: &Arc<Mutex<WebOsTestRuntime>>,
    stop: &AtomicBool,
) where
    S: Read + Write,
{
    {
        let mut runtime = runtime.lock().expect("webOS test server state");
        runtime.connection_count += 1;
    }
    let mut permissions = None;

    while !stop.load(Ordering::Acquire) {
        let request = match read_json(&mut socket, stop) {
            Some(request) => request,
            None => return,
        };

        if request["type"] == "register" {
            if !handle_registration(&mut socket, &request, runtime, &mut permissions) {
                return;
            }
            continue;
        }

        let scenario = runtime.lock().expect("webOS test server state").scenario;
        if scenario == WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff {
            let should_interrupt = {
                let mut runtime = runtime.lock().expect("webOS test server state");
                if runtime.restore_session_interruption_injected {
                    false
                } else {
                    runtime.restore_session_interruption_injected = true;
                    true
                }
            };
            if should_interrupt {
                socket
                    .send(Message::Close(None))
                    .expect("interrupt first webOS restore session");
                return;
            }
        }
        if scenario == WebOsTestScenario::StallFirstRequest {
            let should_stall = {
                let mut runtime = runtime.lock().expect("webOS test server state");
                if runtime.stalled_request_injected {
                    false
                } else {
                    runtime.stalled_request_injected = true;
                    true
                }
            };
            if should_stall {
                let _ = read_json(&mut socket, stop);
                return;
            }
        }

        let keep_open = match scenario {
            WebOsTestScenario::StatefulTv
            | WebOsTestScenario::StoredTokenPairingPrompt
            | WebOsTestScenario::PowerStatePermissionDenied
            | WebOsTestScenario::BacklightWriteAcknowledgedWithoutChange
            | WebOsTestScenario::CloseAfterFirstInputWrite
            | WebOsTestScenario::StallFirstRequest
            | WebOsTestScenario::SameInputWriteAcknowledgedWhileScreenOff
            | WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff => {
                let response = {
                    let mut runtime = runtime.lock().expect("webOS test server state");
                    if scenario == WebOsTestScenario::PowerStatePermissionDenied
                        && request["uri"] == GET_POWER_STATE_URI
                    {
                        Some(webos_error_without_payload(
                            request["id"].as_str().expect("webOS request ID"),
                            "-401 not permitted",
                        ))
                    } else if scenario == WebOsTestScenario::CloseAfterFirstInputWrite
                        && request["uri"] == SET_INPUT_URI
                        && !runtime.ambiguous_input_write_injected
                    {
                        runtime.tv.handle_request(
                            &request,
                            permissions
                                .as_ref()
                                .expect("webOS request sent before registration"),
                            true,
                            false,
                        );
                        runtime.ambiguous_input_write_injected = true;
                        None
                    } else {
                        Some(runtime.tv.handle_request(
                                &request,
                                permissions
                                    .as_ref()
                                    .expect("webOS request sent before registration"),
                                scenario
                                    != WebOsTestScenario::BacklightWriteAcknowledgedWithoutChange,
                                matches!(
                                    scenario,
                                    WebOsTestScenario::SameInputWriteAcknowledgedWhileScreenOff
                                        | WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff
                                ),
                            ))
                    }
                };
                match response {
                    Some(response) => {
                        send_json(&mut socket, response);
                        true
                    }
                    None => false,
                }
            }
            _ => handle_protocol_scenario(&mut socket, scenario, &request),
        };
        if !keep_open {
            return;
        }
    }
}

fn handle_registration<S>(
    socket: &mut WebSocket<S>,
    request: &Value,
    runtime: &Arc<Mutex<WebOsTestRuntime>>,
    permissions: &mut Option<WebOsTestPermissions>,
) -> bool
where
    S: Read + Write,
{
    let (scenario, power_state) = {
        let runtime = runtime.lock().expect("webOS test server state");
        (runtime.scenario, runtime.tv.power_state.clone())
    };
    if power_state == WebOsPowerState::PowerOff {
        socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "Try Again Later (EWS)".into(),
            })))
            .expect("send post-power-off close");
        return false;
    }

    let request_id = request["id"].as_str().expect("registration request ID");
    let payload = request["payload"]
        .as_object()
        .expect("registration request payload");
    let manifest = payload["manifest"]
        .as_object()
        .expect("registration manifest");
    let top_level = permission_set(
        manifest
            .get("permissions")
            .expect("registration permissions"),
        "registration permission",
    );
    let write_settings_authorized = has_observed_write_settings_envelope(manifest);
    *permissions = Some(WebOsTestPermissions {
        top_level,
        write_settings_authorized,
    });
    let presented_token = match payload.get("client-key") {
        Some(Value::String(token)) => Some(token.clone()),
        Some(Value::Null) => None,
        other => panic!("registration client key is neither a string nor null: {other:?}"),
    };
    runtime
        .lock()
        .expect("webOS test server state")
        .registration_tokens
        .push(presented_token);

    match scenario {
        WebOsTestScenario::StatefulTv
        | WebOsTestScenario::PowerStatePermissionDenied
        | WebOsTestScenario::BacklightWriteAcknowledgedWithoutChange
        | WebOsTestScenario::CloseAfterFirstInputWrite
        | WebOsTestScenario::StallFirstRequest
        | WebOsTestScenario::SameInputWriteAcknowledgedWhileScreenOff
        | WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff => {
            match payload["client-key"].as_str() {
                Some(token) => send_json(socket, registration_response(request_id, token)),
                None if payload["client-key"].is_null() => {
                    record_pairing_prompt(runtime);
                    send_json(socket, pairing_prompt_response(request_id));
                    send_json(socket, registration_response(request_id, TEST_ACCESS_TOKEN));
                }
                None => panic!("registration access token is neither a string nor null"),
            }
        }
        WebOsTestScenario::StoredTokenReplacement => {
            payload["client-key"]
                .as_str()
                .expect("stored-token replacement scenario requires a client key");
            send_json(
                socket,
                registration_response(request_id, TEST_REPLACEMENT_ACCESS_TOKEN),
            );
        }
        WebOsTestScenario::StoredTokenPairingPrompt => match payload["client-key"].as_str() {
            Some(_) => {
                record_pairing_prompt(runtime);
                send_json(socket, pairing_prompt_response(request_id));
            }
            None if payload["client-key"].is_null() => {
                record_pairing_prompt(runtime);
                send_json(socket, pairing_prompt_response(request_id));
                send_json(socket, registration_response(request_id, TEST_ACCESS_TOKEN));
            }
            None => panic!("registration access token is neither a string nor null"),
        },
        WebOsTestScenario::PairingRejected => {
            assert!(payload["client-key"].is_null());
            send_json(
                socket,
                response(
                    request_id,
                    json!({"returnValue": false, "errorText": "pairing denied"}),
                ),
            );
        }
        WebOsTestScenario::RegistrationTimeout => {}
        WebOsTestScenario::RegistrationMissingClientKey => {
            assert!(payload["client-key"].is_null());
            send_json(
                socket,
                json!({
                    "id": request_id,
                    "type": "registered",
                    "payload": {},
                }),
            );
        }
        WebOsTestScenario::ProtocolEcho
        | WebOsTestScenario::UnrelatedFrameBeforeResponse
        | WebOsTestScenario::WrongResponseId
        | WebOsTestScenario::CloseBeforeResponse
        | WebOsTestScenario::MalformedTextFrame
        | WebOsTestScenario::WebOsError => {
            panic!("protocol scenario `{scenario:?}` does not accept registration")
        }
    }

    true
}

fn handle_protocol_scenario<S>(
    socket: &mut WebSocket<S>,
    scenario: WebOsTestScenario,
    request: &Value,
) -> bool
where
    S: Read + Write,
{
    assert_eq!(request["type"], "request", "expected webOS request frame");
    assert_eq!(request["payload"], json!({}));
    let request_id = request["id"].as_str().expect("webOS request ID");

    match scenario {
        WebOsTestScenario::ProtocolEcho => {
            let uri = request["uri"].as_str().expect("webOS request URI");
            let payload = if uri == "ssap://test/wss" {
                json!({"encrypted": true})
            } else {
                let sequence = uri
                    .strip_prefix("ssap://test/")
                    .expect("protocol echo URI")
                    .parse::<u64>()
                    .expect("protocol echo sequence");
                assert_eq!(request_id, format!("request_{sequence}"));
                json!({"sequence": sequence})
            };
            send_json(socket, response(request_id, payload));
            true
        }
        WebOsTestScenario::UnrelatedFrameBeforeResponse => {
            send_json(socket, response("subscription_0", json!({})));
            send_json(socket, response(request_id, json!({"ok": true})));
            true
        }
        WebOsTestScenario::WrongResponseId => {
            send_json(socket, response("wrong", json!({"ok": true})));
            true
        }
        WebOsTestScenario::CloseBeforeResponse => {
            socket
                .send(Message::Close(None))
                .expect("send webOS test close");
            false
        }
        WebOsTestScenario::MalformedTextFrame => {
            socket
                .send(Message::text("not-json"))
                .expect("send malformed webOS test frame");
            false
        }
        WebOsTestScenario::WebOsError => {
            send_json(
                socket,
                webos_error_without_payload(request_id, "-401 not permitted"),
            );
            true
        }
        WebOsTestScenario::StatefulTv
        | WebOsTestScenario::StoredTokenReplacement
        | WebOsTestScenario::StoredTokenPairingPrompt
        | WebOsTestScenario::PairingRejected
        | WebOsTestScenario::RegistrationTimeout
        | WebOsTestScenario::RegistrationMissingClientKey
        | WebOsTestScenario::PowerStatePermissionDenied
        | WebOsTestScenario::BacklightWriteAcknowledgedWithoutChange
        | WebOsTestScenario::CloseAfterFirstInputWrite
        | WebOsTestScenario::StallFirstRequest
        | WebOsTestScenario::SameInputWriteAcknowledgedWhileScreenOff
        | WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff => {
            panic!("scenario `{scenario:?}` requires registered stateful handling")
        }
    }
}

fn read_json<S>(socket: &mut WebSocket<S>, stop: &AtomicBool) -> Option<Value>
where
    S: Read + Write,
{
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return Some(serde_json::from_str(text.as_str()).expect("webOS test request JSON"))
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return None,
            Ok(other) => panic!("expected webOS text request, got {other:?}"),
            Err(WebSocketError::Io(_)) if stop.load(Ordering::Acquire) => return None,
            Err(WebSocketError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return None
            }
            Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => return None,
            Err(WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => {
                return None
            }
            Err(error) => panic!("read webOS test request: {error}"),
        }
    }
}

fn send_json<S>(socket: &mut WebSocket<S>, value: Value)
where
    S: Read + Write,
{
    socket
        .send(Message::text(value.to_string()))
        .expect("send webOS test response");
}

fn test_tls_config() -> Arc<ServerConfig> {
    let certificate = CertificateDer::from(
        STANDARD
            .decode(TEST_CERTIFICATE_DER)
            .expect("decode webOS test certificate"),
    );
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        STANDARD
            .decode(TEST_PRIVATE_KEY_DER)
            .expect("decode webOS test private key"),
    ));
    let provider = Arc::new(ring::default_provider());
    Arc::new(
        ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], private_key)
            .expect("build webOS test TLS config"),
    )
}

fn permission_set(value: &Value, description: &str) -> HashSet<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{description}s must be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{description} must be a string"))
                .to_string()
        })
        .collect()
}

// Real-TV observations show that this exact signed envelope authorizes WRITE_SETTINGS.
// Mutating either its signed payload or signature removes that authorization.
fn has_observed_write_settings_envelope(manifest: &serde_json::Map<String, Value>) -> bool {
    let actual = json!({
        "appVersion": manifest.get("appVersion"),
        "signatures": manifest.get("signatures"),
        "signed": manifest.get("signed"),
    });
    actual == *write_settings_signed_envelope()
}

fn write_settings_signed_envelope() -> &'static Value {
    WRITE_SETTINGS_SIGNED_ENVELOPE.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/web_os/test_support/write_settings_signed_envelope.json"
        )))
        .expect("parse observed WRITE_SETTINGS registration envelope")
    })
}

fn record_pairing_prompt(runtime: &Arc<Mutex<WebOsTestRuntime>>) {
    runtime
        .lock()
        .expect("webOS test server state")
        .pairing_prompt_count += 1;
}

fn require_top_level_permission(permissions: &WebOsTestPermissions, permission: &str, uri: &str) {
    assert!(
        permissions.top_level.contains(permission),
        "no missing-permission observation exists for `{uri}`; expected `{permission}`"
    );
}

fn response(request_id: &str, payload: Value) -> Value {
    json!({
        "id": request_id,
        "type": "response",
        "payload": payload,
    })
}

fn registration_response(request_id: &str, access_token: &str) -> Value {
    json!({
        "id": request_id,
        "type": "registered",
        "payload": {"client-key": access_token},
    })
}

fn pairing_prompt_response(request_id: &str) -> Value {
    json!({
        "id": request_id,
        "type": "response",
        "payload": {
            "pairingType": "PROMPT",
            "returnValue": true,
        },
    })
}

fn webos_error(request_id: &str, error: &str, payload: Value) -> Value {
    json!({
        "id": request_id,
        "type": "error",
        "error": error,
        "payload": payload,
    })
}

fn webos_error_without_payload(request_id: &str, error: &str) -> Value {
    json!({
        "id": request_id,
        "type": "error",
        "error": error,
    })
}

pub(in crate::web_os) struct TestAccessTokenStore {
    path: PathBuf,
    store: PlatformAccessTokenStore,
}

impl TestAccessTokenStore {
    pub(in crate::web_os) fn new() -> Self {
        let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-webos-test-server-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create webOS test token directory");
        #[cfg(unix)]
        let owner = SystemUser::new(
            "test-user",
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
            &path,
        );
        #[cfg(not(unix))]
        let owner = SystemUser::new("test-user", 0, 0, &path);
        let store = PlatformAccessTokenStore::for_primary_profile(&path.join("config.env"), owner)
            .expect("construct webOS test token store");
        store
            .persist(&PlatformAccessToken::new(TEST_ACCESS_TOKEN).expect("webOS test access token"))
            .expect("persist webOS test access token");
        Self { path, store }
    }

    pub(in crate::web_os) fn store(&self) -> &PlatformAccessTokenStore {
        &self.store
    }
}

impl Drop for TestAccessTokenStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::{
        write_settings_signed_envelope, WebOsTestInput, WebOsTestScenario, WebOsTestServer,
        GET_SYSTEM_SETTINGS_URI, SET_SYSTEM_SETTINGS_URI,
    };
    use serde_json::{json, Value};
    use std::net::TcpStream;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{connect, Message, WebSocket};

    type TestClient = WebSocket<MaybeTlsStream<TcpStream>>;

    fn connect_registered(
        server: &WebOsTestServer,
        top_level_permissions: &[&str],
        signed_envelope: Option<Value>,
    ) -> TestClient {
        let (mut socket, _) = connect(server.endpoint().to_string()).expect("connect test client");
        let mut manifest = json!({
            "manifestVersion": 1,
            "permissions": top_level_permissions,
        });
        if let Some(Value::Object(envelope)) = signed_envelope {
            manifest
                .as_object_mut()
                .expect("test registration manifest")
                .extend(envelope);
        }
        socket
            .send(Message::text(
                json!({
                    "id": "register_0",
                    "type": "register",
                    "payload": {
                        "client-key": "test-client-key",
                        "forcePairing": false,
                        "manifest": manifest,
                        "pairingType": "PROMPT",
                    },
                })
                .to_string(),
            ))
            .expect("send test registration");
        assert_eq!(
            read_json(&mut socket),
            json!({
                "id": "register_0",
                "type": "registered",
                "payload": {"client-key": "test-client-key"},
            })
        );
        socket
    }

    fn exchange(socket: &mut TestClient, request_id: &str, uri: &str, payload: Value) -> Value {
        socket
            .send(Message::text(
                json!({
                    "id": request_id,
                    "type": "request",
                    "uri": uri,
                    "payload": payload,
                })
                .to_string(),
            ))
            .expect("send test request");
        read_json(socket)
    }

    fn read_json(socket: &mut TestClient) -> Value {
        match socket.read().expect("read test response") {
            Message::Text(text) => serde_json::from_str(text.as_str()).expect("test response JSON"),
            other => panic!("expected text response, got {other:?}"),
        }
    }

    fn read_backlight(socket: &mut TestClient, request_id: &str) -> Value {
        exchange(
            socket,
            request_id,
            GET_SYSTEM_SETTINGS_URI,
            json!({"category": "picture", "keys": ["backlight"]}),
        )["payload"]["settings"]["backlight"]
            .clone()
    }

    // Real-TV wire observation:
    // https://github.com/Staphylococcus/LG_Buddy/issues/52#issuecomment-5183221492
    #[test]
    fn observed_write_settings_envelope_changes_backlight_to_numeric_state() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi2);
        let mut socket = connect_registered(
            &server,
            &["READ_SETTINGS"],
            Some(write_settings_signed_envelope().clone()),
        );

        assert_eq!(read_backlight(&mut socket, "request_0"), json!("90"));
        assert_eq!(
            exchange(
                &mut socket,
                "request_1",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            ),
            json!({
                "id": "request_1",
                "type": "response",
                "payload": {"method": "setSystemSettings", "returnValue": true},
            })
        );
        assert_eq!(read_backlight(&mut socket, "request_2"), json!(75));

        drop(socket);
        server.finish();
    }

    // Real-TV wire observation: mutating a byte inside the signature removed authorization.
    // https://github.com/Staphylococcus/LG_Buddy/issues/52#issuecomment-5183221492
    #[test]
    fn tampered_observed_signature_does_not_authorize_backlight_write() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi2);
        let mut envelope = write_settings_signed_envelope().clone();
        let signature = envelope["signatures"][0]["signature"]
            .as_str()
            .expect("observed signature");
        let encoded_signature_start = signature.rfind('.').expect("signature separator") + 1;
        let replacement = if signature.as_bytes()[encoded_signature_start] == b'A' {
            "B"
        } else {
            "A"
        };
        let mut tampered_signature = signature.to_string();
        tampered_signature.replace_range(
            encoded_signature_start..=encoded_signature_start,
            replacement,
        );
        envelope["signatures"][0]["signature"] = json!(tampered_signature);
        let mut socket = connect_registered(&server, &["READ_SETTINGS"], Some(envelope));

        assert_eq!(
            exchange(
                &mut socket,
                "request_0",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            ),
            json!({
                "id": "request_0",
                "type": "error",
                "error": "401 insufficient permissions",
                "payload": {},
            })
        );
        assert_eq!(read_backlight(&mut socket, "request_1"), json!("90"));

        drop(socket);
        server.finish();
    }

    // Real-TV wire observation: reducing the signed permission list removed authorization.
    // https://github.com/Staphylococcus/LG_Buddy/issues/52#issuecomment-5183221492
    #[test]
    fn reduced_observed_signed_permissions_do_not_authorize_backlight_write() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi2);
        let mut envelope = write_settings_signed_envelope().clone();
        envelope["signed"]["permissions"] = json!(["WRITE_SETTINGS"]);
        let mut socket = connect_registered(&server, &["READ_SETTINGS"], Some(envelope));

        assert_eq!(
            exchange(
                &mut socket,
                "request_0",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            ),
            json!({
                "id": "request_0",
                "type": "error",
                "error": "401 insufficient permissions",
                "payload": {},
            })
        );
        assert_eq!(read_backlight(&mut socket, "request_1"), json!("90"));

        drop(socket);
        server.finish();
    }

    // Real-TV wire observation:
    // https://github.com/Staphylococcus/LG_Buddy/issues/52#issuecomment-5181831148
    #[test]
    fn top_level_write_settings_permission_does_not_authorize_backlight_write() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi2);
        let mut socket = connect_registered(&server, &["READ_SETTINGS", "WRITE_SETTINGS"], None);

        assert_eq!(
            exchange(
                &mut socket,
                "request_0",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            ),
            json!({
                "id": "request_0",
                "type": "error",
                "error": "401 insufficient permissions",
                "payload": {},
            })
        );
        assert_eq!(read_backlight(&mut socket, "request_1"), json!("90"));

        drop(socket);
        server.finish();
    }

    // Real-TV wire observation: a bare signed permission claim remained unauthorized.
    // https://github.com/Staphylococcus/LG_Buddy/issues/52#issuecomment-5181933079
    #[test]
    fn minimal_signed_write_settings_permission_does_not_authorize_backlight_write() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi2);
        let mut socket = connect_registered(
            &server,
            &["READ_SETTINGS"],
            Some(json!({"signed": {"permissions": ["WRITE_SETTINGS"]}})),
        );

        assert_eq!(
            exchange(
                &mut socket,
                "request_0",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            ),
            json!({
                "id": "request_0",
                "type": "error",
                "error": "401 insufficient permissions",
                "payload": {},
            })
        );
        assert_eq!(read_backlight(&mut socket, "request_1"), json!("90"));

        drop(socket);
        server.finish();
    }

    // Synthetic fault injection for defensive readback verification.
    #[test]
    fn acknowledged_backlight_write_can_leave_state_unchanged() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestScenario::BacklightWriteAcknowledgedWithoutChange,
        );
        let mut socket = connect_registered(
            &server,
            &["READ_SETTINGS"],
            Some(write_settings_signed_envelope().clone()),
        );

        assert_eq!(
            exchange(
                &mut socket,
                "request_0",
                SET_SYSTEM_SETTINGS_URI,
                json!({"category": "picture", "settings": {"backlight": 75}}),
            )["payload"],
            json!({"method": "setSystemSettings", "returnValue": true})
        );
        assert_eq!(read_backlight(&mut socket, "request_1"), json!(100));

        drop(socket);
        server.finish();
    }
}
