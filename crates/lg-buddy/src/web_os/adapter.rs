use super::{
    WebOsAuthenticatedClientError, WebOsBacklightBrightness, WebOsBacklightBrightnessError,
    WebOsClient, WebOsClientError, WebOsControlError, WebOsEndpoint, WebOsForegroundAppError,
    WebOsPowerState, WebOsPowerStateError, WebOsScreenControlError,
    WebOsSetBacklightBrightnessError,
};
use crate::platform_access_token::{PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore};
use crate::tv::{CurrentInput, OledBrightness, TvClient, TvError, TvErrorKind, TvOperation};
use serde_json::Value;
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub struct WebOsTvClient {
    endpoint: WebOsEndpoint,
    connect_timeout: Duration,
    response_timeout: Duration,
    token_store: PlatformAccessTokenStore,
    session: Mutex<Option<WebOsClient>>,
}

impl WebOsTvClient {
    pub fn new(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        token_store: PlatformAccessTokenStore,
    ) -> Self {
        Self {
            endpoint,
            connect_timeout,
            response_timeout,
            token_store,
            session: Mutex::new(None),
        }
    }

    fn with_session<T>(
        &self,
        operation: TvOperation,
        action: impl FnOnce(&mut WebOsClient) -> Result<T, WebOsAdapterFailure>,
    ) -> Result<T, TvError> {
        let mut session = self.lock_session(operation)?;
        self.ensure_session(operation, &mut session)?;

        let result = action(
            session
                .as_mut()
                .expect("webOS session was initialized before the operation"),
        );
        match result {
            Ok(value) => Ok(value),
            Err(failure) => {
                if failure.invalidates_session {
                    *session = None;
                }
                Err(failure.into_tv_error(operation))
            }
        }
    }

    fn lock_session(
        &self,
        operation: TvOperation,
    ) -> Result<MutexGuard<'_, Option<WebOsClient>>, TvError> {
        self.session.lock().map_err(|_| {
            TvError::new(
                operation,
                TvErrorKind::Internal,
                "webOS session lock is poisoned",
            )
        })
    }

    fn ensure_session(
        &self,
        operation: TvOperation,
        session: &mut Option<WebOsClient>,
    ) -> Result<(), TvError> {
        if session.is_some() {
            return Ok(());
        }

        let client = WebOsClient::connect_authenticated_with_stored_token(
            self.endpoint,
            self.connect_timeout,
            self.response_timeout,
            &self.token_store,
        )
        .map_err(|error| authenticated_client_failure(error).into_tv_error(operation))?;
        *session = Some(client);
        Ok(())
    }

    fn power_off_active_session(&self) -> Result<(), TvError> {
        let operation = TvOperation::PowerOff;
        let mut session = self.lock_session(operation)?;
        self.ensure_session(operation, &mut session)?;

        let power_state = match session
            .as_mut()
            .expect("webOS session was initialized before the operation")
            .power_state()
        {
            Ok(power_state) => power_state,
            Err(error) => {
                let failure = power_state_failure(error);
                if failure.invalidates_session {
                    *session = None;
                }
                return Err(failure.into_tv_error(operation));
            }
        };

        if power_state != WebOsPowerState::Active {
            return Err(TvError::new(
                operation,
                TvErrorKind::Rejected,
                format!(
                    "native webOS power-off requires power state `Active`, but the TV reported `{power_state}`"
                ),
            ));
        }

        let client = session
            .take()
            .expect("webOS session was initialized before power-off");
        client
            .power_off()
            .map_err(|error| control_failure(error).into_tv_error(operation))
    }
}

impl TvClient for WebOsTvClient {
    fn current_input(&self) -> Result<CurrentInput, TvError> {
        self.with_session(TvOperation::ReadInput, |client| {
            client
                .foreground_app()
                .map(|app| CurrentInput::from_raw(app.app_id().to_string()))
                .map_err(foreground_app_failure)
        })
    }

    fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        self.with_session(TvOperation::ReadOledBrightness, |client| {
            let brightness = client
                .backlight_brightness()
                .map_err(backlight_brightness_failure)?;
            OledBrightness::new(brightness.as_percent()).map_err(|error| {
                WebOsAdapterFailure::new(
                    TvErrorKind::InvalidResponse,
                    format!("native webOS returned invalid OLED brightness: {error}"),
                    false,
                )
            })
        })
    }

    fn set_input(&self, input: crate::config::HdmiInput) -> Result<(), TvError> {
        let input_id = super::WebOsInputId::new(input.as_str()).map_err(|error| {
            TvError::new(
                TvOperation::SetInput,
                TvErrorKind::Internal,
                format!("could not map configured input to webOS: {error}"),
            )
        })?;
        self.with_session(TvOperation::SetInput, |client| {
            client.switch_input(&input_id).map_err(control_failure)
        })
    }

    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        let webos_brightness =
            WebOsBacklightBrightness::new(brightness.as_percent()).map_err(|error| {
                TvError::new(
                    TvOperation::SetOledBrightness,
                    TvErrorKind::Internal,
                    format!("could not map OLED brightness to webOS: {error}"),
                )
            })?;
        self.with_session(TvOperation::SetOledBrightness, |client| {
            client
                .set_backlight_brightness(webos_brightness)
                .map_err(set_backlight_brightness_failure)
        })
    }

    fn power_off(&self) -> Result<(), TvError> {
        self.power_off_active_session()
    }

    fn blank_screen(&self) -> Result<(), TvError> {
        self.with_session(TvOperation::BlankScreen, |client| {
            client
                .turn_screen_off()
                .map(|_| ())
                .map_err(screen_control_failure)
        })
    }

    fn unblank_screen(&self) -> Result<(), TvError> {
        self.with_session(TvOperation::UnblankScreen, |client| {
            client.turn_screen_on().map(|_| ()).map_err(|error| {
                if screen_unblank_substate_mismatch(&error) {
                    WebOsAdapterFailure::new(
                        TvErrorKind::ScreenUnblankSubstateMismatch,
                        error.to_string(),
                        false,
                    )
                } else {
                    screen_control_failure(error)
                }
            })
        })
    }
}

struct WebOsAdapterFailure {
    kind: TvErrorKind,
    detail: String,
    invalidates_session: bool,
}

impl WebOsAdapterFailure {
    fn new(kind: TvErrorKind, detail: impl Into<String>, invalidates_session: bool) -> Self {
        Self {
            kind,
            detail: detail.into(),
            invalidates_session,
        }
    }

    fn into_tv_error(self, operation: TvOperation) -> TvError {
        TvError::new(operation, self.kind, self.detail)
    }
}

fn authenticated_client_failure(error: WebOsAuthenticatedClientError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsAuthenticatedClientError::Connect { source } => {
            client_failure_with_detail(source, detail)
        }
        WebOsAuthenticatedClientError::Authentication { source } => match source {
            PlatformAccessTokenAcquisitionError::MissingStoredToken => {
                WebOsAdapterFailure::new(TvErrorKind::Authentication, detail, false)
            }
            PlatformAccessTokenAcquisitionError::Registration { source } => match source {
                super::WebOsClientRegistrationError::Transport { source } => {
                    client_failure_with_detail(source, detail)
                }
                super::WebOsClientRegistrationError::Protocol { .. }
                | super::WebOsClientRegistrationError::StoredTokenRequiresPairing => {
                    WebOsAdapterFailure::new(TvErrorKind::Authentication, detail, false)
                }
            },
            PlatformAccessTokenAcquisitionError::Store { .. } => {
                WebOsAdapterFailure::new(TvErrorKind::Authentication, detail, false)
            }
        },
    }
}

fn client_failure_with_detail(
    error: WebOsClientError,
    detail: impl Into<String>,
) -> WebOsAdapterFailure {
    let detail = detail.into();
    match error {
        WebOsClientError::WebOs { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsClientError::InvalidTimeout { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Internal, detail, false)
        }
        WebOsClientError::MalformedJson { .. }
        | WebOsClientError::InvalidFrameRoot
        | WebOsClientError::MissingResponseId
        | WebOsClientError::InvalidResponseId
        | WebOsClientError::MissingMessageType
        | WebOsClientError::InvalidMessageType
        | WebOsClientError::MissingWebOsErrorMessage
        | WebOsClientError::InvalidWebOsErrorMessage
        | WebOsClientError::UnexpectedBinaryFrame
        | WebOsClientError::UnexpectedRawFrame => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, true)
        }
        WebOsClientError::Connect { .. }
        | WebOsClientError::ConfigureSocket { .. }
        | WebOsClientError::Handshake { .. }
        | WebOsClientError::HandshakeInterrupted
        | WebOsClientError::RequestIdExhausted
        | WebOsClientError::Send { .. }
        | WebOsClientError::Timeout { .. }
        | WebOsClientError::ConnectionClosed { .. }
        | WebOsClientError::Receive { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Transport, detail, true)
        }
    }
}

fn foreground_app_failure(error: WebOsForegroundAppError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsForegroundAppError::Request { source } => client_failure_with_detail(source, detail),
        WebOsForegroundAppError::RequestRejected { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsForegroundAppError::MissingPayload
        | WebOsForegroundAppError::InvalidPayload
        | WebOsForegroundAppError::MissingReturnValue
        | WebOsForegroundAppError::InvalidReturnValue
        | WebOsForegroundAppError::MissingAppId
        | WebOsForegroundAppError::InvalidAppId
        | WebOsForegroundAppError::EmptyAppId => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn power_state_failure(error: WebOsPowerStateError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsPowerStateError::Request { source } => client_failure_with_detail(source, detail),
        WebOsPowerStateError::RequestRejected { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsPowerStateError::MissingPayload
        | WebOsPowerStateError::InvalidPayload
        | WebOsPowerStateError::MissingReturnValue
        | WebOsPowerStateError::InvalidReturnValue
        | WebOsPowerStateError::MissingState
        | WebOsPowerStateError::InvalidState
        | WebOsPowerStateError::EmptyState => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn control_failure(error: WebOsControlError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsControlError::Request { source } => client_failure_with_detail(source, detail),
        WebOsControlError::RequestRejected { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsControlError::MissingPayload
        | WebOsControlError::InvalidPayload
        | WebOsControlError::MissingReturnValue
        | WebOsControlError::InvalidReturnValue => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn screen_control_failure(error: WebOsScreenControlError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsScreenControlError::Control { source } => {
            let mut failure = control_failure(source);
            failure.detail = detail;
            failure
        }
        WebOsScreenControlError::MissingState
        | WebOsScreenControlError::InvalidState
        | WebOsScreenControlError::EmptyState => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn backlight_brightness_failure(error: WebOsBacklightBrightnessError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsBacklightBrightnessError::Request { source } => {
            client_failure_with_detail(source, detail)
        }
        WebOsBacklightBrightnessError::RequestRejected { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsBacklightBrightnessError::MissingPayload
        | WebOsBacklightBrightnessError::InvalidPayload
        | WebOsBacklightBrightnessError::MissingReturnValue
        | WebOsBacklightBrightnessError::InvalidReturnValue
        | WebOsBacklightBrightnessError::MissingSettings
        | WebOsBacklightBrightnessError::InvalidSettings
        | WebOsBacklightBrightnessError::MissingBacklight
        | WebOsBacklightBrightnessError::InvalidBacklight { .. }
        | WebOsBacklightBrightnessError::BacklightOutOfRange { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn set_backlight_brightness_failure(
    error: WebOsSetBacklightBrightnessError,
) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsSetBacklightBrightnessError::Control { source } => {
            let mut failure = control_failure(source);
            failure.detail = detail;
            failure
        }
        WebOsSetBacklightBrightnessError::Readback { source, .. } => {
            let mut failure = backlight_brightness_failure(source);
            failure.detail = detail;
            failure
        }
        WebOsSetBacklightBrightnessError::NotApplied { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsSetBacklightBrightnessError::MissingMethod
        | WebOsSetBacklightBrightnessError::InvalidMethod
        | WebOsSetBacklightBrightnessError::UnexpectedMethod { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

fn screen_unblank_substate_mismatch(error: &WebOsScreenControlError) -> bool {
    let WebOsScreenControlError::Control { source } = error else {
        return false;
    };
    match source {
        WebOsControlError::Request {
            source:
                WebOsClientError::WebOs {
                    payload: Some(payload),
                    ..
                },
        }
        | WebOsControlError::RequestRejected { payload, .. } => payload_error_code(payload) == -102,
        _ => false,
    }
}

fn payload_error_code(payload: &Value) -> i64 {
    payload
        .get("errorCode")
        .and_then(|value| match value {
            Value::Number(number) => number.as_i64(),
            Value::String(value) => value.parse::<i64>().ok(),
            _ => None,
        })
        .unwrap_or_default()
}

impl fmt::Debug for WebOsTvClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebOsTvClient")
            .field("endpoint", &self.endpoint)
            .field("connect_timeout", &self.connect_timeout)
            .field("response_timeout", &self.response_timeout)
            .field("token_store", &self.token_store)
            .field("session", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::WebOsTvClient;
    use crate::config::HdmiInput;
    use crate::tv::{CurrentInput, OledBrightness, SelectedTvClient, TvClient, TvErrorKind};
    use crate::web_os::test_support::{
        TestAccessTokenStore, WebOsTestInput, WebOsTestScenario, WebOsTestServer,
    };
    use crate::web_os::WebOsPowerState;
    use std::fs;
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

    #[test]
    fn missing_runtime_token_returns_actionable_authentication_error() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        fs::remove_file(token_fixture.store().token_path()).expect("remove stored token");
        let client = client_for_server(&server, &token_fixture);

        let error = client
            .current_input()
            .expect_err("background authentication must not initiate pairing");

        assert_eq!(error.kind(), TvErrorKind::Authentication);
        assert!(error
            .detail()
            .contains("pair the TV from foreground settings"));
        server.finish();
    }

    #[test]
    fn rejected_runtime_token_returns_actionable_authentication_error() {
        let server = WebOsTestServer::for_scenario(WebOsTestScenario::StoredTokenPairingPrompt);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        let error = client
            .current_input()
            .expect_err("stale runtime token must not initiate pairing");

        assert_eq!(error.kind(), TvErrorKind::Authentication);
        assert!(error.detail().contains("foreground pairing"));
        server.finish();
    }

    #[test]
    fn complete_tv_contract_reuses_one_authenticated_session() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        let client = SelectedTvClient::WebOs(Box::new(client_for_server(&server, &token_fixture)));

        assert_eq!(server.snapshot().connection_count, 0);

        assert_eq!(
            client.current_input().expect("read current input"),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
        client.set_input(HdmiInput::Hdmi2).expect("switch input");
        assert_eq!(
            client.current_input().expect("read switched input"),
            CurrentInput::Hdmi(HdmiInput::Hdmi2)
        );
        assert_eq!(
            client
                .oled_brightness()
                .expect("read input-specific brightness")
                .as_percent(),
            90
        );
        client
            .set_oled_brightness(brightness(42))
            .expect("set brightness");
        assert_eq!(
            client
                .oled_brightness()
                .expect("read updated brightness")
                .as_percent(),
            42
        );
        client.blank_screen().expect("blank screen");
        client.unblank_screen().expect("unblank screen");

        assert_eq!(server.snapshot().connection_count, 1);
        client.power_off().expect("power off active TV");

        let snapshot = server.snapshot();
        assert_eq!(snapshot.power_state, WebOsPowerState::PowerOff);
        assert_eq!(snapshot.input, WebOsTestInput::Hdmi2);
        assert_eq!(snapshot.connection_count, 1);
        server.finish();
    }

    #[test]
    fn ambiguous_write_is_not_replayed_and_later_operation_reconnects() {
        let server = WebOsTestServer::for_scenario(WebOsTestScenario::CloseAfterFirstInputWrite);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        assert_eq!(
            client.current_input().expect("establish initial session"),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
        let error = client
            .set_input(HdmiInput::Hdmi2)
            .expect_err("lost write response must be reported");

        assert_eq!(error.kind(), TvErrorKind::Transport);
        let after_failure = server.snapshot();
        assert_eq!(after_failure.input, WebOsTestInput::Hdmi2);
        assert_eq!(
            after_failure.connection_count, 1,
            "the failed effectful operation must not reconnect and replay"
        );

        assert_eq!(
            client.current_input().expect("later operation reconnects"),
            CurrentInput::Hdmi(HdmiInput::Hdmi2)
        );
        assert_eq!(server.snapshot().connection_count, 2);
        server.finish();
    }

    #[test]
    fn screen_unblank_substate_mismatch_is_typed_without_discarding_session() {
        let server = WebOsTestServer::active(WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        let error = client
            .unblank_screen()
            .expect_err("active screen cannot be unblanked");
        assert_eq!(error.kind(), TvErrorKind::ScreenUnblankSubstateMismatch);
        assert_eq!(
            client
                .current_input()
                .expect("application rejection keeps healthy session"),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    #[test]
    fn power_off_requires_active_state_and_keeps_rejected_session_usable() {
        let server = WebOsTestServer::screen_off(WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        let error = client
            .power_off()
            .expect_err("screen-off state fails native power-off guard");
        assert_eq!(error.kind(), TvErrorKind::Rejected);
        client
            .unblank_screen()
            .expect("guard rejection keeps session usable");
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    fn client_for_server(
        server: &WebOsTestServer,
        token_fixture: &TestAccessTokenStore,
    ) -> WebOsTvClient {
        WebOsTvClient::new(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            token_fixture.store().clone(),
        )
    }

    fn brightness(value: u8) -> OledBrightness {
        OledBrightness::new(value).expect("valid test brightness")
    }
}
