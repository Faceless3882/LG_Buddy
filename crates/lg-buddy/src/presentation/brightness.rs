use crate::tv::{OledBrightness, OLED_BRIGHTNESS_MAX, OLED_BRIGHTNESS_MIN};

const BRIGHTNESS_STEP: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessPresentation {
    title: String,
    heading: String,
    status: BrightnessStatus,
    control: Option<BrightnessControl>,
    primary_action: Option<ActionPresentation>,
    cancel_action: ActionPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrightnessStatus {
    Loading { message: String },
    Ready { message: String },
    Applying { message: String },
    Failed(UserFacingError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessControl {
    label: String,
    current: OledBrightness,
    proposed: OledBrightness,
    minimum: u8,
    maximum: u8,
    step: u8,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPresentation {
    label: String,
    enabled: bool,
    intent: BrightnessIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessIntent {
    Propose(u8),
    Apply,
    Retry,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrightnessFrontendUpdate {
    Present(BrightnessPresentation),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacingError {
    summary: String,
    detail: String,
}

impl BrightnessPresentation {
    pub fn loading() -> Self {
        Self::new(
            BrightnessStatus::Loading {
                message: "Loading current brightness…".to_string(),
            },
            None,
            None,
        )
    }

    pub(crate) fn ready(current: OledBrightness, proposed: OledBrightness) -> Self {
        Self::new(
            BrightnessStatus::Ready {
                message: format!("Current brightness: {current}%"),
            },
            Some(BrightnessControl {
                label: "OLED Pixel Brightness".to_string(),
                current,
                proposed,
                minimum: OLED_BRIGHTNESS_MIN,
                maximum: OLED_BRIGHTNESS_MAX,
                step: BRIGHTNESS_STEP,
                enabled: true,
            }),
            Some(ActionPresentation::new(
                "Apply",
                proposed != current,
                BrightnessIntent::Apply,
            )),
        )
    }

    pub(crate) fn applying(current: OledBrightness, proposed: OledBrightness) -> Self {
        Self::new(
            BrightnessStatus::Applying {
                message: format!("Applying brightness: {proposed}%…"),
            },
            Some(BrightnessControl {
                label: "OLED Pixel Brightness".to_string(),
                current,
                proposed,
                minimum: OLED_BRIGHTNESS_MIN,
                maximum: OLED_BRIGHTNESS_MAX,
                step: BRIGHTNESS_STEP,
                enabled: false,
            }),
            Some(ActionPresentation::new(
                "Apply",
                false,
                BrightnessIntent::Apply,
            )),
        )
    }

    pub(crate) fn read_failed(error: UserFacingError) -> Self {
        Self::new(
            BrightnessStatus::Failed(error),
            None,
            Some(ActionPresentation::new(
                "Retry",
                true,
                BrightnessIntent::Retry,
            )),
        )
    }

    pub(crate) fn write_failed(
        current: OledBrightness,
        proposed: OledBrightness,
        error: UserFacingError,
    ) -> Self {
        Self::new(
            BrightnessStatus::Failed(error),
            Some(BrightnessControl {
                label: "OLED Pixel Brightness".to_string(),
                current,
                proposed,
                minimum: OLED_BRIGHTNESS_MIN,
                maximum: OLED_BRIGHTNESS_MAX,
                step: BRIGHTNESS_STEP,
                enabled: true,
            }),
            Some(ActionPresentation::new(
                "Retry",
                proposed != current,
                BrightnessIntent::Retry,
            )),
        )
    }

    fn new(
        status: BrightnessStatus,
        control: Option<BrightnessControl>,
        primary_action: Option<ActionPresentation>,
    ) -> Self {
        Self {
            title: "LG TV Brightness".to_string(),
            heading: "OLED Pixel Brightness".to_string(),
            status,
            control,
            primary_action,
            cancel_action: ActionPresentation::new("Cancel", true, BrightnessIntent::Cancel),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn heading(&self) -> &str {
        &self.heading
    }

    pub fn status(&self) -> &BrightnessStatus {
        &self.status
    }

    pub fn control(&self) -> Option<&BrightnessControl> {
        self.control.as_ref()
    }

    pub fn primary_action(&self) -> Option<&ActionPresentation> {
        self.primary_action.as_ref()
    }

    pub fn cancel_action(&self) -> &ActionPresentation {
        &self.cancel_action
    }
}

impl BrightnessControl {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn current(&self) -> OledBrightness {
        self.current
    }

    pub fn proposed(&self) -> OledBrightness {
        self.proposed
    }

    pub fn minimum(&self) -> u8 {
        self.minimum
    }

    pub fn maximum(&self) -> u8 {
        self.maximum
    }

    pub fn step(&self) -> u8 {
        self.step
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

impl ActionPresentation {
    fn new(label: &str, enabled: bool, intent: BrightnessIntent) -> Self {
        Self {
            label: label.to_string(),
            enabled,
            intent,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn intent(&self) -> BrightnessIntent {
        self.intent
    }
}

impl UserFacingError {
    pub(crate) fn new(summary: &str, detail: &str) -> Self {
        Self {
            summary: summary.to_string(),
            detail: detail.to_string(),
        }
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[cfg(test)]
mod tests {
    use super::{BrightnessIntent, BrightnessPresentation, BrightnessStatus};
    use crate::tv::OledBrightness;

    #[test]
    fn loading_presentation_declares_the_initial_brightness_screen() {
        let presentation = BrightnessPresentation::loading();

        assert_eq!(presentation.title(), "LG TV Brightness");
        assert_eq!(presentation.heading(), "OLED Pixel Brightness");
        assert_eq!(
            presentation.status(),
            &BrightnessStatus::Loading {
                message: "Loading current brightness…".to_string(),
            }
        );
        assert!(presentation.control().is_none());
        assert!(presentation.primary_action().is_none());
        assert_eq!(presentation.cancel_action().label(), "Cancel");
        assert!(presentation.cancel_action().enabled());
        assert_eq!(
            presentation.cancel_action().intent(),
            BrightnessIntent::Cancel
        );
    }

    #[test]
    fn ready_presentation_declares_an_editable_control_and_apply_availability() {
        let brightness = OledBrightness::new(72).expect("valid brightness");
        let presentation = BrightnessPresentation::ready(brightness, brightness);
        let control = presentation.control().expect("ready control");

        assert_eq!(
            presentation.status(),
            &BrightnessStatus::Ready {
                message: "Current brightness: 72%".to_string(),
            }
        );
        assert_eq!(control.label(), "OLED Pixel Brightness");
        assert_eq!(control.current(), brightness);
        assert_eq!(control.proposed(), brightness);
        assert_eq!(control.minimum(), 0);
        assert_eq!(control.maximum(), 100);
        assert_eq!(control.step(), 5);
        assert!(control.enabled());
        let apply = presentation.primary_action().expect("apply action");
        assert_eq!(apply.label(), "Apply");
        assert!(!apply.enabled());
        assert_eq!(apply.intent(), BrightnessIntent::Apply);

        let proposed = OledBrightness::new(65).expect("valid proposal");
        let changed = BrightnessPresentation::ready(brightness, proposed);
        assert_eq!(
            changed.control().expect("ready control").proposed(),
            proposed
        );
        assert!(changed.primary_action().expect("apply action").enabled());
    }

    #[test]
    fn applying_presentation_keeps_the_captured_value_visible_and_disables_changes() {
        let current = OledBrightness::new(72).expect("valid brightness");
        let proposed = OledBrightness::new(65).expect("valid proposal");
        let presentation = BrightnessPresentation::applying(current, proposed);
        let control = presentation.control().expect("applying control");

        assert_eq!(
            presentation.status(),
            &BrightnessStatus::Applying {
                message: "Applying brightness: 65%…".to_string(),
            }
        );
        assert_eq!(control.current(), current);
        assert_eq!(control.proposed(), proposed);
        assert!(!control.enabled());
        assert!(!presentation
            .primary_action()
            .expect("apply action")
            .enabled());
        assert!(presentation.cancel_action().enabled());
    }
}
