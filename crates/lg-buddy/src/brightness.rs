use std::error::Error;
use std::fmt;

use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::notifications::{FreedesktopNotifier, Notification, NotificationError, Notifier};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessWriteFailure {
    NotConfigured,
    InvalidConfiguration,
    CredentialsUnavailable,
    Unreachable,
    Rejected,
    ScreenNotVisible,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessWriteError {
    failure: BrightnessWriteFailure,
    diagnostic: String,
}

impl BrightnessWriteError {
    pub fn new(failure: BrightnessWriteFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn failure(&self) -> BrightnessWriteFailure {
        self.failure
    }
}

impl fmt::Display for BrightnessWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl Error for BrightnessWriteError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrightnessWriteOutcome {
    Applied,
    AppliedWithoutNotification { diagnostic: String },
}

impl BrightnessWriteOutcome {
    pub fn applied() -> Self {
        Self::Applied
    }

    fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Applied => None,
            Self::AppliedWithoutNotification { diagnostic } => Some(diagnostic),
        }
    }
}

pub trait BrightnessWriter: Send + Sync + 'static {
    fn write_brightness(
        &self,
        brightness: OledBrightness,
    ) -> Result<BrightnessWriteOutcome, BrightnessWriteError>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentBrightnessReader;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentBrightnessWriter;

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

impl BrightnessWriter for EnvironmentBrightnessWriter {
    fn write_brightness(
        &self,
        brightness: OledBrightness,
    ) -> Result<BrightnessWriteOutcome, BrightnessWriteError> {
        let config_path = resolve_config_path_from_env().map_err(|error| {
            BrightnessWriteError::new(BrightnessWriteFailure::NotConfigured, error.to_string())
        })?;
        let config = load_config(&config_path).map_err(|error| {
            BrightnessWriteError::new(
                BrightnessWriteFailure::InvalidConfiguration,
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
            BrightnessWriteError::new(
                BrightnessWriteFailure::CredentialsUnavailable,
                error.to_string(),
            )
        })?;

        write_brightness_and_notify_with(&config, &client, &FreedesktopNotifier, brightness)
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

impl From<TvError> for BrightnessWriteError {
    fn from(error: TvError) -> Self {
        let failure = match error.kind() {
            TvErrorKind::Transport => BrightnessWriteFailure::Unreachable,
            TvErrorKind::Authentication => BrightnessWriteFailure::CredentialsUnavailable,
            TvErrorKind::Rejected => BrightnessWriteFailure::Rejected,
            TvErrorKind::InvalidResponse => BrightnessWriteFailure::Internal,
            TvErrorKind::ScreenNotVisible => BrightnessWriteFailure::ScreenNotVisible,
            TvErrorKind::Internal => BrightnessWriteFailure::Internal,
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

pub(crate) fn write_brightness_with<C: TvClient>(
    config: &Config,
    client: &C,
    brightness: OledBrightness,
) -> Result<(), TvError> {
    TvDevice::new(client, config.tv_ip)
        .picture()
        .set_oled_brightness(brightness)
}

pub(crate) fn notify_brightness_success_with<N: Notifier>(
    notifier: &N,
    brightness: OledBrightness,
) -> Result<(), NotificationError> {
    notifier
        .notify(&Notification::new(
            "LG TV",
            format!("Brightness set to {brightness}%"),
        ))
        .map(|_| ())
}

fn write_brightness_and_notify_with<C: TvClient, N: Notifier>(
    config: &Config,
    client: &C,
    notifier: &N,
    brightness: OledBrightness,
) -> Result<BrightnessWriteOutcome, BrightnessWriteError> {
    write_brightness_with(config, client, brightness).map_err(BrightnessWriteError::from)?;
    Ok(match notify_brightness_success_with(notifier, brightness) {
        Ok(()) => BrightnessWriteOutcome::Applied,
        Err(error) => BrightnessWriteOutcome::AppliedWithoutNotification {
            diagnostic: format!(
                "brightness was set to {brightness}%, but desktop notification failed: {error}"
            ),
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrightnessReadOperation(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrightnessWriteOperation {
    id: u64,
    brightness: OledBrightness,
}

impl BrightnessWriteOperation {
    pub fn brightness(self) -> OledBrightness {
        self.brightness
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessTransition {
    update: BrightnessFrontendUpdate,
    read_operation: Option<BrightnessReadOperation>,
    write_operation: Option<BrightnessWriteOperation>,
    diagnostic: Option<String>,
}

impl BrightnessTransition {
    pub fn update(&self) -> &BrightnessFrontendUpdate {
        &self.update
    }

    pub fn read_operation(&self) -> Option<BrightnessReadOperation> {
        self.read_operation
    }

    pub fn write_operation(&self) -> Option<BrightnessWriteOperation> {
        self.write_operation
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrightnessApplicationState {
    Loading(BrightnessReadOperation),
    Ready {
        current: OledBrightness,
        proposed: OledBrightness,
    },
    ReadFailed,
    Applying {
        current: OledBrightness,
        proposed: OledBrightness,
        operation: BrightnessWriteOperation,
    },
    WriteFailed {
        current: OledBrightness,
        proposed: OledBrightness,
        error: UserFacingError,
    },
    Closed,
}

#[derive(Debug)]
pub struct BrightnessApplication {
    state: BrightnessApplicationState,
    next_operation_id: u64,
}

impl BrightnessApplication {
    pub fn open() -> (Self, BrightnessTransition) {
        let operation = BrightnessReadOperation(0);
        (
            Self {
                state: BrightnessApplicationState::Loading(operation),
                next_operation_id: 1,
            },
            BrightnessTransition {
                update: BrightnessFrontendUpdate::Present(BrightnessPresentation::loading()),
                read_operation: Some(operation),
                write_operation: None,
                diagnostic: None,
            },
        )
    }

    pub fn handle_intent(&mut self, intent: BrightnessIntent) -> Option<BrightnessTransition> {
        match (self.state.clone(), intent) {
            (BrightnessApplicationState::ReadFailed, BrightnessIntent::Retry) => {
                let operation = BrightnessReadOperation(self.next_operation_id);
                self.next_operation_id += 1;
                self.state = BrightnessApplicationState::Loading(operation);
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Present(BrightnessPresentation::loading()),
                    read_operation: Some(operation),
                    write_operation: None,
                    diagnostic: None,
                })
            }
            (
                BrightnessApplicationState::Ready { current, proposed },
                BrightnessIntent::Propose(value),
            ) => self.update_proposal(current, proposed, value, None),
            (
                BrightnessApplicationState::WriteFailed {
                    current,
                    proposed,
                    error,
                },
                BrightnessIntent::Propose(value),
            ) => self.update_proposal(current, proposed, value, Some(error)),
            (BrightnessApplicationState::Ready { current, proposed }, BrightnessIntent::Apply)
                if current != proposed =>
            {
                Some(self.begin_write(current, proposed))
            }
            (
                BrightnessApplicationState::WriteFailed {
                    current, proposed, ..
                },
                BrightnessIntent::Retry,
            ) if current != proposed => Some(self.begin_write(current, proposed)),
            (state, BrightnessIntent::Cancel) if state != BrightnessApplicationState::Closed => {
                self.state = BrightnessApplicationState::Closed;
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Close,
                    read_operation: None,
                    write_operation: None,
                    diagnostic: None,
                })
            }
            _ => None,
        }
    }

    fn update_proposal(
        &mut self,
        current: OledBrightness,
        previous: OledBrightness,
        value: u8,
        error: Option<UserFacingError>,
    ) -> Option<BrightnessTransition> {
        let proposed = OledBrightness::new(value).ok()?;
        if proposed == previous {
            return None;
        }

        let presentation = if let Some(error) = error {
            self.state = BrightnessApplicationState::WriteFailed {
                current,
                proposed,
                error: error.clone(),
            };
            BrightnessPresentation::write_failed(current, proposed, error)
        } else {
            self.state = BrightnessApplicationState::Ready { current, proposed };
            BrightnessPresentation::ready(current, proposed)
        };
        Some(BrightnessTransition {
            update: BrightnessFrontendUpdate::Present(presentation),
            read_operation: None,
            write_operation: None,
            diagnostic: None,
        })
    }

    fn begin_write(
        &mut self,
        current: OledBrightness,
        proposed: OledBrightness,
    ) -> BrightnessTransition {
        let operation = BrightnessWriteOperation {
            id: self.next_operation_id,
            brightness: proposed,
        };
        self.next_operation_id += 1;
        self.state = BrightnessApplicationState::Applying {
            current,
            proposed,
            operation,
        };
        BrightnessTransition {
            update: BrightnessFrontendUpdate::Present(BrightnessPresentation::applying(
                current, proposed,
            )),
            read_operation: None,
            write_operation: Some(operation),
            diagnostic: None,
        }
    }

    pub fn complete_read(
        &mut self,
        operation: BrightnessReadOperation,
        result: Result<OledBrightness, BrightnessReadError>,
    ) -> Option<BrightnessTransition> {
        if !matches!(self.state, BrightnessApplicationState::Loading(active) if active == operation)
        {
            return None;
        }

        let (presentation, diagnostic) = match result {
            Ok(brightness) => {
                self.state = BrightnessApplicationState::Ready {
                    current: brightness,
                    proposed: brightness,
                };
                (BrightnessPresentation::ready(brightness, brightness), None)
            }
            Err(error) => {
                self.state = BrightnessApplicationState::ReadFailed;
                let presentation =
                    BrightnessPresentation::read_failed(user_facing_read_error(error.failure()));
                let diagnostic = format!("could not load current brightness: {error}");
                (presentation, Some(diagnostic))
            }
        };
        Some(BrightnessTransition {
            update: BrightnessFrontendUpdate::Present(presentation),
            read_operation: None,
            write_operation: None,
            diagnostic,
        })
    }

    pub fn complete_write(
        &mut self,
        operation: BrightnessWriteOperation,
        result: Result<BrightnessWriteOutcome, BrightnessWriteError>,
    ) -> Option<BrightnessTransition> {
        let BrightnessApplicationState::Applying {
            current,
            proposed,
            operation: active,
        } = self.state.clone()
        else {
            return None;
        };
        if active != operation {
            return None;
        }

        match result {
            Ok(outcome) => {
                self.state = BrightnessApplicationState::Closed;
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Close,
                    read_operation: None,
                    write_operation: None,
                    diagnostic: outcome.diagnostic().map(str::to_string),
                })
            }
            Err(error) => {
                let user_error = user_facing_write_error(error.failure());
                self.state = BrightnessApplicationState::WriteFailed {
                    current,
                    proposed,
                    error: user_error.clone(),
                };
                Some(BrightnessTransition {
                    update: BrightnessFrontendUpdate::Present(
                        BrightnessPresentation::write_failed(current, proposed, user_error),
                    ),
                    read_operation: None,
                    write_operation: None,
                    diagnostic: Some(format!("could not apply brightness: {error}")),
                })
            }
        }
    }

    pub fn shutdown(&mut self) {
        self.state = BrightnessApplicationState::Closed;
    }
}

fn user_facing_read_error(failure: BrightnessReadFailure) -> UserFacingError {
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

fn user_facing_write_error(failure: BrightnessWriteFailure) -> UserFacingError {
    let (summary, detail) = match failure {
        BrightnessWriteFailure::NotConfigured => (
            "LG Buddy is not configured.",
            "Complete LG Buddy setup, then retry.",
        ),
        BrightnessWriteFailure::InvalidConfiguration => (
            "LG Buddy could not load its TV configuration.",
            "Check the saved TV address and platform settings, then retry.",
        ),
        BrightnessWriteFailure::CredentialsUnavailable => (
            "LG Buddy cannot authenticate with this TV.",
            "Run `lg-buddy brightness set <0-100>` in a terminal, accept a TV pairing prompt if shown, then retry.",
        ),
        BrightnessWriteFailure::Unreachable => (
            "The TV could not be reached.",
            "Make sure the TV is on and connected to the same network, then retry.",
        ),
        BrightnessWriteFailure::Rejected => (
            "The TV rejected the brightness change.",
            "Make sure the TV screen is on, then retry.",
        ),
        BrightnessWriteFailure::ScreenNotVisible => (
            "The TV screen is not available.",
            "Turn the TV screen on, then retry.",
        ),
        BrightnessWriteFailure::Internal => (
            "LG Buddy could not set the brightness.",
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
        read_current_brightness_with, write_brightness_and_notify_with, BrightnessApplication,
        BrightnessReadError, BrightnessReadFailure, BrightnessWriteError, BrightnessWriteFailure,
        BrightnessWriteOutcome,
    };
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenIdleBlankPolicy, ScreenRestorePolicy,
        SystemSleepWakePolicy, TvPlatform,
    };
    use crate::notifications::{
        Notification, NotificationCapabilities, NotificationError, NotificationId, Notifier,
    };
    use crate::presentation::brightness::{
        BrightnessFrontendUpdate, BrightnessIntent, BrightnessStatus,
    };
    use crate::tv::{BscpylgtvCommandClient, OledBrightness, TvErrorKind};
    use std::cell::RefCell;
    use std::net::Ipv4Addr;
    use support::MockBscpylgtv;

    #[derive(Default)]
    struct RecordingNotifier {
        messages: RefCell<Vec<(String, String)>>,
        failure: Option<&'static str>,
    }

    impl RecordingNotifier {
        fn failing(message: &'static str) -> Self {
            Self {
                messages: RefCell::new(Vec::new()),
                failure: Some(message),
            }
        }

        fn messages(&self) -> Vec<(String, String)> {
            self.messages.borrow().clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError> {
            Ok(NotificationCapabilities { actions: false })
        }

        fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
            self.messages
                .borrow_mut()
                .push((notification.summary.clone(), notification.body.clone()));
            if let Some(message) = self.failure {
                Err(NotificationError::Transport(message.to_string()))
            } else {
                Ok(NotificationId(1))
            }
        }
    }

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
        assert!(transition.write_operation().is_none());
    }

    #[test]
    fn proposal_is_validated_and_does_not_start_a_write() {
        let mut application = ready_application(72);

        assert!(application
            .handle_intent(BrightnessIntent::Propose(101))
            .is_none());
        assert!(application.handle_intent(BrightnessIntent::Apply).is_none());

        let transition = application
            .handle_intent(BrightnessIntent::Propose(65))
            .expect("valid proposal transition");
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("proposal should present ready state");
        };
        let control = presentation.control().expect("ready control");

        assert!(matches!(
            presentation.status(),
            BrightnessStatus::Ready { .. }
        ));
        assert_eq!(control.current().as_percent(), 72);
        assert_eq!(control.proposed().as_percent(), 65);
        assert!(presentation
            .primary_action()
            .expect("apply action")
            .enabled());
        assert!(transition.read_operation().is_none());
        assert!(transition.write_operation().is_none());
        assert!(application
            .handle_intent(BrightnessIntent::Propose(65))
            .is_none());
    }

    #[test]
    fn apply_captures_one_proposal_and_publishes_busy_state() {
        let mut application = ready_application(72);
        application
            .handle_intent(BrightnessIntent::Propose(65))
            .expect("valid proposal");

        let transition = application
            .handle_intent(BrightnessIntent::Apply)
            .expect("apply transition");
        let operation = transition.write_operation().expect("write operation");
        let BrightnessFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("apply should present applying state");
        };

        assert_eq!(operation.brightness().as_percent(), 65);
        assert!(matches!(
            presentation.status(),
            BrightnessStatus::Applying { .. }
        ));
        assert!(!presentation.control().expect("applying control").enabled());
        assert!(!presentation
            .primary_action()
            .expect("apply action")
            .enabled());
        assert!(application.handle_intent(BrightnessIntent::Apply).is_none());
        assert!(application
            .handle_intent(BrightnessIntent::Propose(40))
            .is_none());
        assert!(application.handle_intent(BrightnessIntent::Retry).is_none());
    }

    #[test]
    fn successful_write_closes_and_ignores_duplicate_completion() {
        let (mut application, operation) = applying_application(72, 65);

        let completion = application
            .complete_write(operation, Ok(BrightnessWriteOutcome::Applied))
            .expect("current write completion");

        assert_eq!(completion.update(), &BrightnessFrontendUpdate::Close);
        assert!(completion.diagnostic().is_none());
        assert!(application
            .complete_write(operation, Ok(BrightnessWriteOutcome::Applied))
            .is_none());
    }

    #[test]
    fn notification_warning_does_not_turn_an_applied_write_into_a_retry() {
        let (mut application, operation) = applying_application(72, 65);

        let completion = application
            .complete_write(
                operation,
                Ok(BrightnessWriteOutcome::AppliedWithoutNotification {
                    diagnostic: "notification bus unavailable".to_string(),
                }),
            )
            .expect("current write completion");

        assert_eq!(completion.update(), &BrightnessFrontendUpdate::Close);
        assert_eq!(
            completion.diagnostic(),
            Some("notification bus unavailable")
        );
        assert!(application.handle_intent(BrightnessIntent::Retry).is_none());
    }

    #[test]
    fn failed_write_preserves_proposal_and_retry_replaces_the_operation() {
        let (mut application, old_operation) = applying_application(72, 65);
        let failure = application
            .complete_write(
                old_operation,
                Err(BrightnessWriteError::new(
                    BrightnessWriteFailure::Unreachable,
                    "sensitive write diagnostic",
                )),
            )
            .expect("write failure transition");
        let BrightnessFrontendUpdate::Present(presentation) = failure.update() else {
            panic!("write failure should remain open");
        };
        let BrightnessStatus::Failed(error) = presentation.status() else {
            panic!("write failure should present an error");
        };

        assert!(error.summary().contains("could not be reached"));
        assert!(!error.summary().contains("sensitive"));
        assert!(!error.detail().contains("sensitive"));
        assert_eq!(
            presentation
                .control()
                .expect("failed write control")
                .proposed()
                .as_percent(),
            65
        );
        assert_eq!(
            presentation
                .primary_action()
                .expect("retry action")
                .intent(),
            BrightnessIntent::Retry
        );
        assert!(failure
            .diagnostic()
            .is_some_and(|value| value.contains("sensitive write diagnostic")));

        let retry = application
            .handle_intent(BrightnessIntent::Retry)
            .expect("retry transition");
        let new_operation = retry.write_operation().expect("retry write");
        assert_ne!(old_operation, new_operation);
        assert_eq!(new_operation.brightness().as_percent(), 65);
        assert!(application
            .complete_write(old_operation, Ok(BrightnessWriteOutcome::Applied))
            .is_none());
        assert!(application
            .complete_write(new_operation, Ok(BrightnessWriteOutcome::Applied))
            .is_some());
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
    fn cancellation_before_apply_closes_without_a_write() {
        let mut application = ready_application(72);
        application
            .handle_intent(BrightnessIntent::Propose(65))
            .expect("valid proposal");

        let cancellation = application
            .handle_intent(BrightnessIntent::Cancel)
            .expect("cancel transition");

        assert_eq!(cancellation.update(), &BrightnessFrontendUpdate::Close);
        assert!(cancellation.write_operation().is_none());
        assert!(application.handle_intent(BrightnessIntent::Apply).is_none());
    }

    #[test]
    fn cancellation_during_write_closes_and_ignores_late_completion() {
        let (mut application, operation) = applying_application(72, 65);

        let cancellation = application
            .handle_intent(BrightnessIntent::Cancel)
            .expect("cancel transition");

        assert_eq!(cancellation.update(), &BrightnessFrontendUpdate::Close);
        assert!(application
            .complete_write(operation, Ok(BrightnessWriteOutcome::Applied))
            .is_none());
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
    fn shutdown_ignores_late_write_completion() {
        let (mut application, operation) = applying_application(72, 65);

        application.shutdown();

        assert!(application
            .complete_write(operation, Ok(BrightnessWriteOutcome::Applied))
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
    fn successful_write_uses_the_tv_picture_capability_and_notifies() {
        let config = sample_config();
        let mock = MockBscpylgtv::new("gui-brightness-write");
        let client = client_for_mock(&mock, config.tv_ip);
        let notifier = RecordingNotifier::default();
        let brightness = OledBrightness::new(65).expect("valid brightness");

        let outcome = write_brightness_and_notify_with(&config, &client, &notifier, brightness)
            .expect("brightness write should succeed");

        assert_eq!(outcome, BrightnessWriteOutcome::Applied);
        assert_eq!(mock.state_snapshot().backlight, 65);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| call.command)
                .collect::<Vec<_>>(),
            vec!["set_settings"]
        );
        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Brightness set to 65%".to_string())]
        );
    }

    #[test]
    fn failed_write_is_typed_and_does_not_send_a_success_notification() {
        let config = sample_config();
        let client = BscpylgtvCommandClient::new(
            config.tv_ip,
            "/definitely/missing/lg-buddy-bscpylgtvcommand",
        );
        let notifier = RecordingNotifier::default();

        let error = write_brightness_and_notify_with(
            &config,
            &client,
            &notifier,
            OledBrightness::new(65).expect("valid brightness"),
        )
        .expect_err("brightness write should fail");

        assert_eq!(error.failure(), BrightnessWriteFailure::Unreachable);
        assert!(notifier.messages().is_empty());
    }

    #[test]
    fn notification_failure_preserves_the_successful_tv_write() {
        let config = sample_config();
        let mock = MockBscpylgtv::new("gui-brightness-notification-failure");
        let client = client_for_mock(&mock, config.tv_ip);
        let notifier = RecordingNotifier::failing("bus unavailable");

        let outcome = write_brightness_and_notify_with(
            &config,
            &client,
            &notifier,
            OledBrightness::new(65).expect("valid brightness"),
        )
        .expect("TV write should remain successful");

        assert!(matches!(
            outcome,
            BrightnessWriteOutcome::AppliedWithoutNotification { diagnostic }
                if diagnostic.contains("bus unavailable")
        ));
        assert_eq!(mock.state_snapshot().backlight, 65);
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

    fn ready_application(value: u8) -> BrightnessApplication {
        let (mut application, opening) = BrightnessApplication::open();
        let operation = opening.read_operation().expect("opening read");
        application
            .complete_read(
                operation,
                Ok(OledBrightness::new(value).expect("valid brightness")),
            )
            .expect("ready transition");
        application
    }

    fn applying_application(
        current: u8,
        proposed: u8,
    ) -> (BrightnessApplication, super::BrightnessWriteOperation) {
        let mut application = ready_application(current);
        application
            .handle_intent(BrightnessIntent::Propose(proposed))
            .expect("proposal transition");
        let applying = application
            .handle_intent(BrightnessIntent::Apply)
            .expect("apply transition");
        let operation = applying.write_operation().expect("write operation");
        (application, operation)
    }
}
