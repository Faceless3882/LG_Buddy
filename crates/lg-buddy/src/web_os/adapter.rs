use super::{
    WebOsAudioStatusError, WebOsAudioVolume, WebOsAuthenticatedClientError,
    WebOsBacklightBrightness, WebOsBacklightBrightnessError, WebOsClient, WebOsClientError,
    WebOsControlError, WebOsEndpoint, WebOsForegroundAppError, WebOsPowerState,
    WebOsPowerStateError, WebOsScreenControlError, WebOsSetBacklightBrightnessError,
};
use crate::platform_access_token::{PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore};
use crate::tv::{
    AudioStatus, CurrentInput, CurrentVolume, OledBrightness, TvClient, TvError, TvErrorKind,
    TvOperation, VolumeLevel,
};
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebOsPairingPolicy {
    PairIfNeeded,
    StoredTokenOnly,
}

pub struct WebOsTvClient {
    endpoint: WebOsEndpoint,
    connect_timeout: Duration,
    response_timeout: Duration,
    token_store: PlatformAccessTokenStore,
    pairing_policy: WebOsPairingPolicy,
    session: Mutex<Option<WebOsClient>>,
}

impl WebOsTvClient {
    pub(crate) fn new(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        token_store: PlatformAccessTokenStore,
        pairing_policy: WebOsPairingPolicy,
    ) -> Self {
        Self {
            endpoint,
            connect_timeout,
            response_timeout,
            token_store,
            pairing_policy,
            session: Mutex::new(None),
        }
    }

    pub(crate) fn has_stored_access_token(
        &self,
    ) -> Result<bool, crate::platform_access_token::PlatformAccessTokenStoreError> {
        self.token_store.load().map(|token| token.is_some())
    }

    #[cfg(test)]
    pub(crate) fn response_timeout(&self) -> Duration {
        self.response_timeout
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

        let client = match self.pairing_policy {
            WebOsPairingPolicy::PairIfNeeded => WebOsClient::connect_authenticated(
                self.endpoint,
                self.connect_timeout,
                self.response_timeout,
                &self.token_store,
                |_| {},
            ),
            WebOsPairingPolicy::StoredTokenOnly => {
                WebOsClient::connect_authenticated_with_stored_token(
                    self.endpoint,
                    self.connect_timeout,
                    self.response_timeout,
                    &self.token_store,
                )
            }
        }
        .map_err(|error| authenticated_client_failure(error).into_tv_error(operation))?;
        *session = Some(client);
        Ok(())
    }

    fn power_off_controllable_session(&self) -> Result<(), TvError> {
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

        if !matches!(
            power_state,
            WebOsPowerState::Active | WebOsPowerState::ScreenOff
        ) {
            return Err(TvError::new(
                operation,
                TvErrorKind::Rejected,
                format!(
                    "native webOS power-off requires power state `Active` or `Screen Off`, but the TV reported `{power_state}`"
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

    fn observe_power_state(&self, operation: TvOperation) -> Result<WebOsPowerState, TvError> {
        self.with_session(operation, |client| {
            client.power_state().map_err(power_state_failure)
        })
    }

    fn verify_power_state(
        &self,
        operation: TvOperation,
        expected: WebOsPowerState,
        command_result: Result<(), TvError>,
    ) -> Result<(), TvError> {
        if command_result.as_ref().is_err_and(verification_cannot_help) {
            return command_result;
        }

        match self.observe_power_state(operation) {
            Ok(actual) if actual == expected => Ok(()),
            Ok(actual) => {
                let kind = if expected == WebOsPowerState::Active {
                    TvErrorKind::ScreenNotVisible
                } else {
                    TvErrorKind::Rejected
                };
                Err(postcondition_failure(
                    operation,
                    kind,
                    &command_result,
                    format!("expected power state `{expected}`, but the TV reported `{actual}`"),
                ))
            }
            Err(verification_error) => match command_result {
                Ok(()) => Err(verification_error),
                Err(command_error) => Err(combined_failure(command_error, verification_error)),
            },
        }
    }

    fn verify_input(
        &self,
        input: crate::config::HdmiInput,
        command_result: Result<(), TvError>,
    ) -> Result<(), TvError> {
        let operation = TvOperation::SetInput;
        if command_result.as_ref().is_err_and(verification_cannot_help) {
            return command_result;
        }

        match self.observe_power_state(operation) {
            Ok(WebOsPowerState::Active) => {}
            Ok(actual) => {
                return Err(postcondition_failure(
                    operation,
                    TvErrorKind::ScreenNotVisible,
                    &command_result,
                    format!("cannot verify input while the TV reports power state `{actual}`"),
                ));
            }
            Err(verification_error) => {
                return match command_result {
                    Ok(()) => Err(verification_error),
                    Err(command_error) => Err(combined_failure(command_error, verification_error)),
                };
            }
        }

        let observed_input = self.with_session(operation, |client| {
            client
                .foreground_app()
                .map(|app| CurrentInput::from_raw(app.app_id().to_string()))
                .map_err(foreground_app_failure)
        });
        match observed_input {
            Ok(actual) if actual.is_hdmi(input) => Ok(()),
            Ok(actual) => Err(postcondition_failure(
                operation,
                TvErrorKind::Rejected,
                &command_result,
                format!(
                    "expected input `{}`, but the TV reported `{actual}`",
                    input.as_str()
                ),
            )),
            Err(verification_error) => match command_result {
                Ok(()) => Err(verification_error),
                Err(command_error) => Err(combined_failure(command_error, verification_error)),
            },
        }
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

    fn audio_status(&self) -> Result<AudioStatus, TvError> {
        self.with_session(TvOperation::ReadAudioStatus, |client| {
            let status = client.audio_status().map_err(audio_status_failure)?;
            let volume = match status.volume() {
                WebOsAudioVolume::Known(value) => {
                    let volume = VolumeLevel::new(value).map_err(|error| {
                        WebOsAdapterFailure::new(
                            TvErrorKind::InvalidResponse,
                            format!("native webOS returned invalid volume: {error}"),
                            false,
                        )
                    })?;
                    CurrentVolume::Level(volume)
                }
                WebOsAudioVolume::Unknown => CurrentVolume::Unknown,
            };

            Ok(AudioStatus::new(volume, status.is_muted()))
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
        let command_result = self.with_session(TvOperation::SetInput, |client| {
            client.switch_input(&input_id).map_err(control_failure)
        });
        self.verify_input(input, command_result)
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

    fn set_volume(&self, volume: VolumeLevel) -> Result<(), TvError> {
        self.with_session(TvOperation::SetVolume, |client| {
            client
                .set_volume(volume.as_percent())
                .map_err(control_failure)
        })
    }

    fn volume_up(&self) -> Result<(), TvError> {
        self.with_session(TvOperation::VolumeUp, |client| {
            client.volume_up().map_err(control_failure)
        })
    }

    fn volume_down(&self) -> Result<(), TvError> {
        self.with_session(TvOperation::VolumeDown, |client| {
            client.volume_down().map_err(control_failure)
        })
    }

    fn set_muted(&self, muted: bool) -> Result<(), TvError> {
        self.with_session(TvOperation::SetMuted, |client| {
            client.set_muted(muted).map_err(control_failure)
        })
    }

    fn power_off(&self) -> Result<(), TvError> {
        self.power_off_controllable_session()
    }

    fn blank_screen(&self) -> Result<(), TvError> {
        let command_result = self.with_session(TvOperation::BlankScreen, |client| {
            client
                .turn_screen_off()
                .map(|_| ())
                .map_err(screen_control_failure)
        });
        self.verify_power_state(
            TvOperation::BlankScreen,
            WebOsPowerState::ScreenOff,
            command_result,
        )
    }

    fn unblank_screen(&self) -> Result<(), TvError> {
        let command_result = self.with_session(TvOperation::UnblankScreen, |client| {
            client
                .turn_screen_on()
                .map(|_| ())
                .map_err(screen_control_failure)
        });
        self.verify_power_state(
            TvOperation::UnblankScreen,
            WebOsPowerState::Active,
            command_result,
        )
    }
}

fn postcondition_failure(
    operation: TvOperation,
    kind: TvErrorKind,
    command_result: &Result<(), TvError>,
    observation: impl AsRef<str>,
) -> TvError {
    let detail = match command_result {
        Ok(()) => format!(
            "native webOS acknowledged the command, but {}",
            observation.as_ref()
        ),
        Err(error) => format!(
            "native webOS command failed: {}; verification found that {}",
            error.detail(),
            observation.as_ref()
        ),
    };
    TvError::new(operation, kind, detail)
}

fn combined_failure(command_error: TvError, verification_error: TvError) -> TvError {
    TvError::new(
        command_error.operation(),
        command_error.kind(),
        format!(
            "native webOS command failed: {}; postcondition verification also failed: {}",
            command_error.detail(),
            verification_error.detail()
        ),
    )
}

fn verification_cannot_help(error: &TvError) -> bool {
    matches!(
        error.kind(),
        TvErrorKind::Authentication | TvErrorKind::Internal
    )
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

fn audio_status_failure(error: WebOsAudioStatusError) -> WebOsAdapterFailure {
    let detail = error.to_string();
    match error {
        WebOsAudioStatusError::Request { source } => client_failure_with_detail(source, detail),
        WebOsAudioStatusError::RequestRejected { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::Rejected, detail, false)
        }
        WebOsAudioStatusError::MissingPayload
        | WebOsAudioStatusError::InvalidPayload
        | WebOsAudioStatusError::MissingReturnValue
        | WebOsAudioStatusError::InvalidReturnValue
        | WebOsAudioStatusError::MissingVolume
        | WebOsAudioStatusError::InvalidVolume { .. }
        | WebOsAudioStatusError::MissingMute
        | WebOsAudioStatusError::InvalidMute => {
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
        WebOsSetBacklightBrightnessError::CreateLunaBridge { source }
        | WebOsSetBacklightBrightnessError::CloseLunaBridge { source } => {
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
        WebOsSetBacklightBrightnessError::MissingAlertId
        | WebOsSetBacklightBrightnessError::InvalidAlertId { .. }
        | WebOsSetBacklightBrightnessError::UnexpectedAlertId { .. } => {
            WebOsAdapterFailure::new(TvErrorKind::InvalidResponse, detail, false)
        }
    }
}

impl fmt::Debug for WebOsTvClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebOsTvClient")
            .field("endpoint", &self.endpoint)
            .field("connect_timeout", &self.connect_timeout)
            .field("response_timeout", &self.response_timeout)
            .field("token_store", &self.token_store)
            .field("pairing_policy", &self.pairing_policy)
            .field("session", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{WebOsPairingPolicy, WebOsTvClient};
    use crate::config::HdmiInput;
    use crate::tv::{
        CurrentInput, CurrentVolume, OledBrightness, SelectedTvClient, TvClient, TvErrorKind,
        VolumeLevel,
    };
    use crate::web_os::test_support::{
        TestAccessTokenStore, WebOsTestInput, WebOsTestScenario, WebOsTestServer, WebOsTestVersion,
    };
    use crate::web_os::WebOsPowerState;
    use std::fs;
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

    #[test]
    fn ordinary_operation_pairs_and_persists_a_missing_token() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        fs::remove_file(token_fixture.store().token_path()).expect("remove stored token");
        let client = client_for_server(&server, &token_fixture);

        let input = client
            .current_input()
            .expect("ordinary operation should pair");

        assert_eq!(input, CurrentInput::Hdmi(HdmiInput::Hdmi3));
        assert_eq!(
            token_fixture.store().load().expect("load acquired token"),
            Some(server.access_token())
        );
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    #[test]
    fn ordinary_operation_repairs_a_rejected_token() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenPairingPrompt,
        );
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        let input = client
            .current_input()
            .expect("ordinary operation should repair stale token");

        assert_eq!(input, CurrentInput::Hdmi(HdmiInput::Hdmi3));
        assert_eq!(
            token_fixture.store().load().expect("load repaired token"),
            Some(server.access_token())
        );
        assert_eq!(server.snapshot().connection_count, 2);
        server.finish();
    }

    #[test]
    fn stored_token_only_operation_returns_immediately_when_token_is_missing() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        fs::remove_file(token_fixture.store().token_path()).expect("remove stored token");
        let client = client_for_server_with_policy(
            &server,
            &token_fixture,
            WebOsPairingPolicy::StoredTokenOnly,
        );

        let error = client
            .current_input()
            .expect_err("stored-token-only operation must not pair");

        assert_eq!(error.kind(), TvErrorKind::Authentication);
        assert_eq!(server.snapshot().connection_count, 0);
        assert!(!token_fixture.store().token_path().exists());
        server.finish();
    }

    #[test]
    fn stored_token_only_operation_does_not_repair_a_rejected_token() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenPairingPrompt,
        );
        let token_fixture = TestAccessTokenStore::new();
        let original = token_fixture
            .store()
            .load()
            .expect("load original token")
            .expect("original token");
        let client = client_for_server_with_policy(
            &server,
            &token_fixture,
            WebOsPairingPolicy::StoredTokenOnly,
        );

        let error = client
            .current_input()
            .expect_err("stored-token-only operation must not repair stale token");

        assert_eq!(error.kind(), TvErrorKind::Authentication);
        assert_eq!(
            token_fixture.store().load().expect("reload original token"),
            Some(original)
        );
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    #[test]
    fn effectful_operation_does_not_retry_a_definitive_authentication_failure() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenPairingPrompt,
        );
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server_with_policy(
            &server,
            &token_fixture,
            WebOsPairingPolicy::StoredTokenOnly,
        );

        let error = client
            .set_input(HdmiInput::Hdmi2)
            .expect_err("stored-token-only operation must not retry authentication");

        assert_eq!(error.kind(), TvErrorKind::Authentication);
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    #[test]
    fn complete_tv_contract_reuses_one_authenticated_session() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
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
        let status = client.audio_status().expect("read audio status");
        assert_eq!(status.volume(), CurrentVolume::Level(volume(20)));
        assert!(!status.is_muted());
        client.set_muted(true).expect("mute audio");
        client.set_volume(volume(19)).expect("set volume");
        let status = client.audio_status().expect("read updated audio status");
        assert_eq!(status.volume(), CurrentVolume::Level(volume(19)));
        assert!(status.is_muted());
        client.set_muted(false).expect("unmute audio");
        client.volume_up().expect("increase volume");
        client.volume_down().expect("decrease volume");
        assert_eq!(server.snapshot().volume, 19);
        client.blank_screen().expect("blank screen");
        assert_eq!(server.snapshot().power_state, WebOsPowerState::ScreenOff);
        client.unblank_screen().expect("unblank screen");
        assert_eq!(server.snapshot().power_state, WebOsPowerState::Active);

        assert_eq!(server.snapshot().connection_count, 1);
        client.power_off().expect("power off active TV");

        let snapshot = server.snapshot();
        assert_eq!(snapshot.power_state, WebOsPowerState::PowerOff);
        assert_eq!(snapshot.input, WebOsTestInput::Hdmi2);
        assert_eq!(snapshot.connection_count, 1);
        server.finish();
    }

    #[test]
    fn audio_status_preserves_tv_reported_unknown_volume() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        server.set_volume(-1);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        let status = client.audio_status().expect("read unknown audio status");

        assert_eq!(status.volume(), CurrentVolume::Unknown);
        assert!(!status.is_muted());
        server.finish();
    }

    #[test]
    fn ambiguous_write_is_not_replayed_and_safe_readback_resolves_success() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::CloseAfterFirstInputWrite,
        );
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        assert_eq!(
            client.current_input().expect("establish initial session"),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
        client
            .set_input(HdmiInput::Hdmi2)
            .expect("safe readback should prove the write succeeded");
        let after_write = server.snapshot();
        assert_eq!(after_write.input, WebOsTestInput::Hdmi2);
        assert_eq!(
            after_write.connection_count, 2,
            "verification reconnects without replaying the effectful operation"
        );

        assert_eq!(
            client
                .current_input()
                .expect("verified session remains reusable"),
            CurrentInput::Hdmi(HdmiInput::Hdmi2)
        );
        assert_eq!(server.snapshot().connection_count, 2);
        server.finish();
    }

    #[test]
    fn unblank_is_idempotent_when_the_screen_is_already_active() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        client
            .unblank_screen()
            .expect("verified active state should satisfy unblank");
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
    fn set_input_reports_screen_not_visible_when_screen_off_ack_changes_nothing() {
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);
        client.blank_screen().expect("blank screen");
        server.set_scenario(WebOsTestScenario::SameInputWriteAcknowledgedWhileScreenOff);

        let error = client
            .set_input(HdmiInput::Hdmi3)
            .expect_err("an acknowledgement without a visible screen is not success");

        assert_eq!(error.kind(), TvErrorKind::ScreenNotVisible);
        assert!(error.detail().contains("power state `Screen Off`"));
        assert_eq!(server.snapshot().power_state, WebOsPowerState::ScreenOff);
        server.finish();
    }

    #[test]
    fn power_off_accepts_screen_off_state() {
        let server = WebOsTestServer::screen_off(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestInput::Hdmi3,
        );
        let token_fixture = TestAccessTokenStore::new();
        let client = client_for_server(&server, &token_fixture);

        assert_eq!(
            client
                .current_input()
                .expect("hardware-characterized foreground input remains readable"),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
        assert_eq!(
            client
                .oled_brightness()
                .expect("hardware-characterized backlight remains readable")
                .as_percent(),
            100
        );
        client
            .power_off()
            .expect("hardware-characterized screen-off power-off should succeed");
        assert_eq!(server.snapshot().power_state, WebOsPowerState::PowerOff);
        assert_eq!(server.snapshot().connection_count, 1);
        server.finish();
    }

    fn client_for_server(
        server: &WebOsTestServer,
        token_fixture: &TestAccessTokenStore,
    ) -> WebOsTvClient {
        client_for_server_with_policy(server, token_fixture, WebOsPairingPolicy::PairIfNeeded)
    }

    fn client_for_server_with_policy(
        server: &WebOsTestServer,
        token_fixture: &TestAccessTokenStore,
        pairing_policy: WebOsPairingPolicy,
    ) -> WebOsTvClient {
        WebOsTvClient::new(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            token_fixture.store().clone(),
            pairing_policy,
        )
    }

    fn brightness(value: u8) -> OledBrightness {
        OledBrightness::new(value).expect("valid test brightness")
    }

    fn volume(value: u8) -> VolumeLevel {
        VolumeLevel::new(value).expect("valid test volume")
    }
}
