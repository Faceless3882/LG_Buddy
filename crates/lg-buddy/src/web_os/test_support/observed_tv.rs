//! Stateful webOS mock limited to behavior captured from the real test TV.

use super::super::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsEndpoint,
    WebOsPowerState,
};
use crate::auth::SystemUser;
use crate::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tungstenite::error::ProtocolError;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::{accept, Error as WebSocketError, Message, WebSocket};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);
const SOCKET_POLL_TIMEOUT: Duration = Duration::from_millis(200);
const TEST_ACCESS_TOKEN: &str = "observed-webos-tv-access-token";

const GET_FOREGROUND_APP_URI: &str = "ssap://com.webos.applicationManager/getForegroundAppInfo";
const GET_POWER_STATE_URI: &str = "ssap://com.webos.service.tvpower/power/getPowerState";
const GET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/getSystemSettings";
const SET_INPUT_URI: &str = "ssap://tv/switchInput";
const TURN_OFF_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";
const TURN_ON_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";
const TURN_OFF_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOffScreen";
const TURN_ON_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOnScreen";
const POWER_OFF_URI: &str = "ssap://system/turnOff";

static TEST_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::web_os) enum ObservedWebOsInput {
    Hdmi2,
    Hdmi3,
}

impl ObservedWebOsInput {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::web_os) struct ObservedWebOsTvSnapshot {
    pub(in crate::web_os) power_state: WebOsPowerState,
    pub(in crate::web_os) input: ObservedWebOsInput,
    pub(in crate::web_os) connection_count: u64,
}

struct ObservedWebOsTv {
    power_state: WebOsPowerState,
    input: ObservedWebOsInput,
    backlight: Value,
}

impl ObservedWebOsTv {
    fn new(power_state: WebOsPowerState, input: ObservedWebOsInput) -> Self {
        Self {
            power_state,
            input,
            backlight: json!(100),
        }
    }

    fn handle_request(&mut self, request: &Value, permissions: &HashSet<String>) -> Value {
        assert_eq!(request["type"], "request", "expected webOS request frame");
        let request_id = request["id"].as_str().expect("webOS request ID");
        let uri = request["uri"].as_str().expect("webOS request URI");
        let payload = request
            .get("payload")
            .expect("webOS request payload must be present");

        match uri {
            GET_POWER_STATE_URI => {
                require_permission(permissions, "READ_POWER_STATE", uri);
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
                if !permissions.contains("READ_RUNNING_APPS") {
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
                if !permissions.contains("READ_SETTINGS") {
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
            SET_INPUT_URI => {
                if !permissions.contains("LAUNCH") {
                    return webos_error(request_id, "401 insufficient permissions", json!({}));
                }
                assert_eq!(self.power_state, WebOsPowerState::Active);
                let input_id = payload["inputId"]
                    .as_str()
                    .expect("switch-input request input ID");
                assert_eq!(payload, &json!({"inputId": input_id}));
                self.input = ObservedWebOsInput::from_input_id(input_id);
                response(request_id, json!({"returnValue": true}))
            }
            TURN_OFF_SCREEN_URI => {
                if !permissions.contains("CONTROL_TV_SCREEN") {
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
                if !permissions.contains("CONTROL_TV_SCREEN") {
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
                require_permission(permissions, "CONTROL_POWER", uri);
                assert_eq!(payload, &json!({}));
                assert_eq!(self.power_state, WebOsPowerState::Active);
                self.power_state = WebOsPowerState::PowerOff;
                response(request_id, json!({"returnValue": true}))
            }
            other => panic!("no real-TV observation exists for webOS URI `{other}`"),
        }
    }
}

struct ObservedWebOsTvRuntime {
    tv: ObservedWebOsTv,
    connection_count: u64,
}

pub(in crate::web_os) struct ObservedWebOsTvServer {
    endpoint: WebOsEndpoint,
    address: std::net::SocketAddr,
    runtime: Arc<Mutex<ObservedWebOsTvRuntime>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ObservedWebOsTvServer {
    pub(in crate::web_os) fn active(input: ObservedWebOsInput) -> Self {
        Self::spawn(WebOsPowerState::Active, input)
    }

    pub(in crate::web_os) fn screen_off(input: ObservedWebOsInput) -> Self {
        Self::spawn(WebOsPowerState::ScreenOff, input)
    }

    fn spawn(power_state: WebOsPowerState, input: ObservedWebOsInput) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind observed webOS TV");
        listener
            .set_nonblocking(true)
            .expect("configure observed webOS listener");
        let address = listener.local_addr().expect("observed webOS address");
        let endpoint = WebOsEndpoint::ws_at(address);
        let runtime = Arc::new(Mutex::new(ObservedWebOsTvRuntime {
            tv: ObservedWebOsTv::new(power_state, input),
            connection_count: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let server_runtime = Arc::clone(&runtime);
        let server_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !server_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if server_stop.load(Ordering::Acquire) {
                            break;
                        }
                        stream
                            .set_read_timeout(Some(SOCKET_POLL_TIMEOUT))
                            .expect("configure observed webOS socket");
                        serve_connection(stream, &server_runtime, &server_stop);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept observed webOS connection: {error}"),
                }
            }
        });

        Self {
            endpoint,
            address,
            runtime,
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
        PlatformAccessToken::new(TEST_ACCESS_TOKEN).expect("observed webOS test access token")
    }

    pub(in crate::web_os) fn snapshot(&self) -> ObservedWebOsTvSnapshot {
        let runtime = self.runtime.lock().expect("observed webOS TV state");
        ObservedWebOsTvSnapshot {
            power_state: runtime.tv.power_state.clone(),
            input: runtime.tv.input,
            connection_count: runtime.connection_count,
        }
    }

    pub(in crate::web_os) fn finish(mut self) {
        self.stop_and_join().expect("observed webOS server thread");
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        handle.join()
    }
}

impl Drop for ObservedWebOsTvServer {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn serve_connection(
    stream: TcpStream,
    runtime: &Arc<Mutex<ObservedWebOsTvRuntime>>,
    stop: &AtomicBool,
) {
    let mut socket = accept(stream).expect("accept observed webOS websocket");
    {
        let mut runtime = runtime.lock().expect("observed webOS TV state");
        runtime.connection_count += 1;
    }
    let mut permissions = None;

    while !stop.load(Ordering::Acquire) {
        let request = match read_json(&mut socket, stop) {
            Some(request) => request,
            None => return,
        };

        if request["type"] == "register" {
            let runtime = runtime.lock().expect("observed webOS TV state");
            if runtime.tv.power_state == WebOsPowerState::PowerOff {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Policy,
                        reason: "Try Again Later (EWS)".into(),
                    })))
                    .expect("send observed post-power-off close");
                return;
            }

            let request_id = request["id"].as_str().expect("registration request ID");
            let payload = request["payload"]
                .as_object()
                .expect("registration request payload");
            let manifest = payload["manifest"]
                .as_object()
                .expect("registration manifest");
            let registered_permissions = manifest["permissions"]
                .as_array()
                .expect("registration permissions")
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("registration permission string")
                        .to_string()
                })
                .collect();
            permissions = Some(registered_permissions);
            match payload["client-key"].as_str() {
                Some(token) => {
                    socket
                        .send(Message::text(
                            registration_response(request_id, token).to_string(),
                        ))
                        .expect("send observed stored-token registration response");
                }
                None if payload["client-key"].is_null() => {
                    socket
                        .send(Message::text(
                            pairing_prompt_response(request_id).to_string(),
                        ))
                        .expect("send observed pairing prompt response");
                    socket
                        .send(Message::text(
                            registration_response(request_id, TEST_ACCESS_TOKEN).to_string(),
                        ))
                        .expect("send observed first-pairing registration response");
                }
                None => panic!("registration access token is neither a string nor null"),
            }
            continue;
        }

        let response = {
            let mut runtime = runtime.lock().expect("observed webOS TV state");
            runtime.tv.handle_request(
                &request,
                permissions
                    .as_ref()
                    .expect("webOS request sent before registration"),
            )
        };
        socket
            .send(Message::text(response.to_string()))
            .expect("send observed webOS response");
    }
}

fn read_json(socket: &mut WebSocket<TcpStream>, stop: &AtomicBool) -> Option<Value> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return Some(
                    serde_json::from_str(text.as_str()).expect("observed webOS request JSON"),
                )
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return None,
            Ok(other) => panic!("expected webOS text request, got {other:?}"),
            Err(WebSocketError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                if stop.load(Ordering::Acquire) {
                    return None;
                }
            }
            Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => return None,
            Err(WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake)) => {
                return None
            }
            Err(error) => panic!("read observed webOS request: {error}"),
        }
    }
}

fn require_permission(permissions: &HashSet<String>, permission: &str, uri: &str) {
    assert!(
        permissions.contains(permission),
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

struct TestAccessTokenStore {
    path: PathBuf,
    store: PlatformAccessTokenStore,
}

impl TestAccessTokenStore {
    fn new() -> Self {
        let sequence = TEST_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-observed-webos-tv-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create observed webOS token directory");
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
            .expect("construct observed webOS token store");
        store
            .persist(
                &PlatformAccessToken::new(TEST_ACCESS_TOKEN)
                    .expect("observed webOS test access token"),
            )
            .expect("persist observed webOS test access token");
        Self { path, store }
    }

    fn store(&self) -> &PlatformAccessTokenStore {
        &self.store
    }
}

impl Drop for TestAccessTokenStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
