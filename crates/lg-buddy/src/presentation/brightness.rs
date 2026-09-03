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

    pub(crate) fn ready(brightness: OledBrightness) -> Self {
        Self::new(
            BrightnessStatus::Ready {
                message: format!("Current brightness: {brightness}%"),
            },
            Some(BrightnessControl {
                label: "OLED Pixel Brightness".to_string(),
                current: brightness,
                proposed: brightness,
                minimum: OLED_BRIGHTNESS_MIN,
                maximum: OLED_BRIGHTNESS_MAX,
                step: BRIGHTNESS_STEP,
                enabled: false,
            }),
            None,
        )
    }

    pub(crate) fn failed(error: UserFacingError) -> Self {
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
    fn ready_presentation_declares_the_validated_read_only_control() {
        let brightness = OledBrightness::new(72).expect("valid brightness");
        let presentation = BrightnessPresentation::ready(brightness);
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
        assert!(!control.enabled());
        assert!(presentation.primary_action().is_none());
    }
}
