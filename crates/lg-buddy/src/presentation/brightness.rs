#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrightnessPresentation {
    title: String,
    heading: String,
    status: BrightnessStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrightnessStatus {
    Loading { message: String },
}

impl BrightnessPresentation {
    pub fn loading() -> Self {
        Self {
            title: "LG TV Brightness".to_string(),
            heading: "OLED Pixel Brightness".to_string(),
            status: BrightnessStatus::Loading {
                message: "Loading current brightness…".to_string(),
            },
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
}

#[cfg(test)]
mod tests {
    use super::{BrightnessPresentation, BrightnessStatus};

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
    }
}
