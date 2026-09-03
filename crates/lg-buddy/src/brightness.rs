use std::error::Error;
use std::fmt;

use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::presentation::brightness::{
    BrightnessFrontendUpdate, BrightnessIntent, BrightnessPresentation, UserFacingError,
};
use crate::tv::{
    build_tv_client, OledBrightness, TvClient, TvClientBuildOptions, TvDevice, TvError, TvErrorKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessReadFailure {
    NotConfigured,
    InvalidConfiguration,
    CredentialsUnavailable,
    Unreachable,
    Rejected,
    InvalidResponse,
    ScreenNotVisible,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessReadError {
    failure: BrightnessReadFailure,
    diagnostic: String,
}

impl BrightnessReadError {
    pub fn new(failure: BrightnessReadFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn failure(&self) -> BrightnessReadFailure {
        self.failure
    }
}

impl fmt::Display for BrightnessReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl Error for BrightnessReadError {}

pub trait BrightnessReader: Send + Sync + 'static {
    fn read_current_brightness(&self) -> Result<OledBrightness, BrightnessReadError>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentBrightnessReader;

impl BrightnessReader for EnvironmentBrightnessReader {
    fn read_current_brightness(&self) -> Result<OledBrightness, BrightnessReadError> {
        let config_path = resolve_config_path_from_env().map_err(|error| {
            BrightnessReadError::new(BrightnessReadFailure::NotConfigured, error.to_string())
        })?;
        let config = load_config(&config_path).map_err(|error| {
            BrightnessReadError::new(
                BrightnessReadFailure::InvalidConfiguration,
                error.to_string(),
            )
        })?;
        let client = build_tv_client(
            &config_path,
            config.tv_ip,
            config.tv_platform,
            TvClientBuildOptions::production().stored_token_only(),
        )
        .map_err(|error| {
            BrightnessReadError::new(
                BrightnessReadFailure::CredentialsUnavailable,
                error.to_string(),
            )
        })?;

        read_current_brightness_with(&config, &client).map_err(BrightnessReadError::from)
    }
}

impl From<TvError> for BrightnessReadError {
    fn from(error: TvError) -> Self {
        let failure = match error.kind() {
            TvErrorKind::Transport => BrightnessReadFailure::Unreachable,
            TvErrorKind::Authentication => BrightnessReadFailure::CredentialsUnavailable,
            TvErrorKind::Rejected => BrightnessReadFailure::Rejected,
            TvErrorKind::InvalidResponse => BrightnessReadFailure::InvalidResponse,
            TvErrorKind::ScreenNotVisible => BrightnessReadFailure::ScreenNotVisible,
            TvErrorKind::Internal => BrightnessReadFailure::Internal,
        };
        Self::new(failure, error.to_string())
    }
}

pub(crate) fn read_current_brightness_with<C: TvClient>(
    config: &Config,
    client: &C,
) -> Result<OledBrightness, TvError> {
    TvDevice::new(client, config.tv_ip)
        .picture()
        .oled_brightness()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrightnessReadOperation(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessTransition {
    update: BrightnessFrontendUpdate,
    read_operation: Option<BrightnessReadOperation>,
    diagnostic: Option<String>,
}

impl BrightnessTransition {
    pub fn update(&self) -> &BrightnessFrontendUpdate {
        &self.update
    }

    pub fn read_operation(&self) -> Option<BrightnessReadOperation> {
        self.read_operation
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrightnessApplicationState {
    Loading,
    Ready,
    Failed,
    Closed,
}

#[derive(Debug)]
pub struct BrightnessApplication {
    state: BrightnessApplicationState,
    active_read: Option<BrightnessReadOperation>,
    next_operation_id: u64,
}

impl BrightnessApplication {
    pub fn open() -> (Self, BrightnessTransition) {
        let operation = BrightnessReadOperation(0);
        (
            Self {
                state: BrightnessApplicationState::Loading,
                active_read: Some(operation),
                next_operation_id: 1,
            },
            BrightnessTransition {
                update: BrightnessFrontendUpdate::Present(BrightnessPresentation::loading()),
                read_operation: Some(operation),
                diagnostic: None,
            },
        )
    }

    pub fn handle_intent(&mut self, intent: BrightnessIntent) -> Option<BrightnessTransition> {
        match intent {
            BrightnessIntent::Retry if self.state == BrightnessApplicationState::Failed => {
                let operation = BrightnessReadOperation(self.next_operation_id);
                self.next_operation_id += 1;
                self.state = BrightnessApplicationState::Loading;
                self.active_read = Some(operation);
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Present(BrightnessPresentation::loading()),
                    read_operation: Some(operation),
                    diagnostic: None,
                })
            }
            BrightnessIntent::Cancel if self.state != BrightnessApplicationState::Closed => {
                self.state = BrightnessApplicationState::Closed;
                self.active_read = None;
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Close,
                    read_operation: None,
                    diagnostic: None,
                })
            }
            BrightnessIntent::Retry | BrightnessIntent::Cancel => None,
        }
    }

    pub fn complete_read(
        &mut self,
        operation: BrightnessReadOperation,
        result: Result<OledBrightness, BrightnessReadError>,
    ) -> Option<BrightnessTransition> {
        if self.state != BrightnessApplicationState::Loading || self.active_read != Some(operation)
        {
            return None;
        }

        self.active_read = None;
        let (presentation, diagnostic) = match result {
            Ok(brightness) => {
                self.state = BrightnessApplicationState::Ready;
                (BrightnessPresentation::ready(brightness), None)
            }
            Err(error) => {
                self.state = BrightnessApplicationState::Failed;
                let presentation =
                    BrightnessPresentation::failed(user_facing_error(error.failure()));
                let diagnostic = format!("could not load current brightness: {error}");
                (presentation, Some(diagnostic))
            }
        };
        Some(BrightnessTransition {
            update: BrightnessFrontendUpdate::Present(presentation),
            read_operation: None,
            diagnostic,
        })
    }

    pub fn shutdown(&mut self) {
        self.state = BrightnessApplicationState::Closed;
        self.active_read = None;
    }
}

fn user_facing_error(failure: BrightnessReadFailure) -> UserFacingError {
    let (summary, detail) = match failure {
        BrightnessReadFailure::NotConfigured => (
            "LG Buddy is not configured.",
            "Complete LG Buddy setup, then retry.",
        ),
        BrightnessReadFailure::InvalidConfiguration => (
            "LG Buddy could not load its TV configuration.",
            "Check the saved TV address and platform settings, then retry.",
        ),
        BrightnessReadFailure::CredentialsUnavailable => (
            "LG Buddy cannot authenticate with this TV.",
            "Run `lg-buddy brightness get` in a terminal, accept a TV pairing prompt if shown, then retry.",
        ),
        BrightnessReadFailure::Unreachable => (
            "The TV could not be reached.",
            "Make sure the TV is on and connected to the same network, then retry.",
        ),
        BrightnessReadFailure::Rejected => (
            "The TV rejected the brightness request.",
            "Make sure the TV screen is on, then retry.",
        ),
        BrightnessReadFailure::InvalidResponse => (
            "The TV returned an invalid brightness value.",
            "Retry. If this continues, check the configured TV platform.",
        ),
        BrightnessReadFailure::ScreenNotVisible => (
            "The TV screen is not available.",
            "Turn the TV screen on, then retry.",
        ),
        BrightnessReadFailure::Internal => (
            "LG Buddy could not read the current brightness.",
            "Retry. If this continues, check the LG Buddy logs.",
        ),
    };
    UserFacingError::new(summary, detail)
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        read_current_brightness_with, BrightnessApplication, BrightnessReadError,
        BrightnessReadFailure,
    };
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenIdleBlankPolicy, ScreenRestorePolicy,
        SystemSleepWakePolicy, TvPlatform,
    };
    use crate::presentation::brightness::{
        BrightnessFrontendUpdate, BrightnessIntent, BrightnessStatus,
    };
    use crate::tv::{BscpylgtvCommandClient, OledBrightness, TvErrorKind};
    use std::net::Ipv4Addr;
    use support::MockBscpylgtv;

    #[test]
    fn opening_publishes_loading_and_one_current_read() {
        let (_application, transition) = BrightnessApplication::open();

        assert!(matches!(
            transition.update(),
            BrightnessFrontendUpdate::Present(presentation)
                if matches!(presentation.status(), BrightnessStatus::Loading { .. })
        ));
        assert!(transition.read_operation().is_some());
    }

    #[test]
    fn successful_read_publishes_the_validated_current_and_proposed_value() {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");
        let brightness = OledBrightness::new(72).expect("valid brightness");

        let transition = application
            .complete_read(operation, Ok(brightness))
            .expect("current completion");
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("successful read should present ready state");
        };
        let control = presentation.control().expect("ready control");

        assert!(matches!(
            presentation.status(),
            BrightnessStatus::Ready { .. }
        ));
        assert_eq!(control.current(), brightness);
        assert_eq!(control.proposed(), brightness);
        assert!(transition.read_operation().is_none());
    }

    #[test]
    fn read_failures_publish_safe_actionable_recovery_presentations() {
        let cases = [
            (
                BrightnessReadFailure::InvalidResponse,
                "invalid brightness value",
                "configured TV platform",
            ),
            (
                BrightnessReadFailure::Unreachable,
                "could not be reached",
                "same network",
            ),
            (
                BrightnessReadFailure::CredentialsUnavailable,
                "cannot authenticate",
                "lg-buddy brightness get",
            ),
        ];

        for (failure, expected_summary, expected_detail) in cases {
            let (mut application, opening) = BrightnessApplication::open();
            let operation = opening.read_operation().expect("opening read");
            let error = BrightnessReadError::new(failure, "sensitive adapter diagnostic");

            let transition = application
                .complete_read(operation, Err(error))
                .expect("current completion");
            let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
                panic!("read failure should present failed state");
            };
            let BrightnessStatus::Failed(error) = presentation.status() else {
                panic!("read failure should declare an error");
            };

            assert!(error.summary().contains(expected_summary));
            assert!(error.detail().contains(expected_detail));
            assert!(!error.summary().contains("sensitive"));
            assert!(!error.detail().contains("sensitive"));
            assert!(transition
                .diagnostic()
                .is_some_and(|diagnostic| diagnostic.contains("sensitive adapter diagnostic")));
            assert_eq!(
                presentation
                    .primary_action()
                    .expect("retry action")
                    .intent(),
                BrightnessIntent::Retry
            );
            assert_eq!(
                presentation.cancel_action().intent(),
                BrightnessIntent::Cancel
            );
        }
    }

    #[test]
    fn retry_starts_a_new_read_and_ignores_the_old_completion() {
        let (mut application, opening) = BrightnessApplication::open();
        let old_operation = opening.read_operation().expect("opening read");
        application
            .complete_read(
                old_operation,
                Err(BrightnessReadError::new(
                    BrightnessReadFailure::Unreachable,
                    "offline",
                )),
            )
            .expect("failed read");

        let retry = application
            .handle_intent(BrightnessIntent::Retry)
            .expect("retry transition");
        let new_operation = retry.read_operation().expect("retry read");

        assert_ne!(old_operation, new_operation);
        assert!(matches!(
            retry.update(),
            BrightnessFrontendUpdate::Present(presentation)
                if matches!(presentation.status(), BrightnessStatus::Loading { .. })
        ));
        assert!(application
            .complete_read(
                old_operation,
                Ok(OledBrightness::new(10).expect("valid brightness"))
            )
            .is_none());
        assert!(application
            .complete_read(
                new_operation,
                Ok(OledBrightness::new(80).expect("valid brightness"))
            )
            .is_some());
    }

    #[test]
    fn cancellation_closes_and_ignores_late_read_completion() {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");

        let cancellation = application
            .handle_intent(BrightnessIntent::Cancel)
            .expect("cancel transition");

        assert_eq!(cancellation.update(), &BrightnessFrontendUpdate::Close);
        assert!(cancellation.read_operation().is_none());
        assert!(application
            .complete_read(
                operation,
                Ok(OledBrightness::new(72).expect("valid brightness"))
            )
            .is_none());
        assert!(application.handle_intent(BrightnessIntent::Retry).is_none());
    }

    #[test]
    fn shutdown_ignores_late_read_completion() {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");

        application.shutdown();

        assert!(application
            .complete_read(
                operation,
                Ok(OledBrightness::new(72).expect("valid brightness"))
            )
            .is_none());
    }

    #[test]
    fn current_read_uses_the_tv_picture_capability() {
        let config = sample_config();
        let mock = MockBscpylgtv::new("gui-brightness-read");
        mock.set_backlight(67);
        let client = client_for_mock(&mock, config.tv_ip);

        let brightness =
            read_current_brightness_with(&config, &client).expect("brightness read should succeed");

        assert_eq!(brightness.as_percent(), 67);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| call.command)
                .collect::<Vec<_>>(),
            vec!["get_picture_settings"]
        );
    }

    #[test]
    fn malformed_tv_brightness_is_a_typed_invalid_response() {
        let config = sample_config();
        let mock = MockBscpylgtv::new("gui-invalid-brightness-read");
        mock.set_backlight(101);
        let client = client_for_mock(&mock, config.tv_ip);

        let error = read_current_brightness_with(&config, &client)
            .expect_err("invalid brightness should fail");
        let application_error = BrightnessReadError::from(error);

        assert_eq!(
            application_error.failure(),
            BrightnessReadFailure::InvalidResponse
        );
    }

    #[test]
    fn transport_failure_is_reported_as_unreachable() {
        let config = sample_config();
        let client = BscpylgtvCommandClient::new(
            config.tv_ip,
            "/definitely/missing/lg-buddy-bscpylgtvcommand",
        );

        let error = read_current_brightness_with(&config, &client)
            .expect_err("missing adapter should fail");
        assert_eq!(error.kind(), TvErrorKind::Transport);
        assert_eq!(
            BrightnessReadError::from(error).failure(),
            BrightnessReadFailure::Unreachable
        );
    }

    fn sample_config() -> Config {
        Config {
            tv_ip: "10.0.0.5".parse().expect("valid IP"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("valid MAC"),
            input: HdmiInput::Hdmi2,
            tv_platform: TvPlatform::Bscpylgtv,
            screen_backend: ScreenBackend::Auto,
            screen_idle_timeout: 300,
            screen_restore_policy: ScreenRestorePolicy::MarkerOnly,
            screen_idle_blank: ScreenIdleBlankPolicy::Enabled,
            system_sleep_wake_policy: SystemSleepWakePolicy::Enabled,
        }
    }

    fn client_for_mock(mock: &MockBscpylgtv, tv_ip: Ipv4Addr) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(tv_ip, mock.command_path(), mock.command_args())
    }
}
