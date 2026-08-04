use super::control::send_control_request;
use super::{WebOsClient, WebOsControlError, WebOsPowerState};
use serde_json::{json, Map, Value};
use std::error::Error;
use std::fmt;

const TURN_OFF_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";
const TURN_ON_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";

#[derive(Debug)]
pub enum WebOsScreenControlError {
    Control { source: WebOsControlError },
    MissingState,
    InvalidState,
    EmptyState,
}

impl fmt::Display for WebOsScreenControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control { source } => write!(f, "webOS screen control failed: {source}"),
            Self::MissingState => write!(f, "webOS screen-control response has no state"),
            Self::InvalidState => {
                write!(f, "webOS screen-control response state is not a string")
            }
            Self::EmptyState => write!(f, "webOS screen-control response state is empty"),
        }
    }
}

impl Error for WebOsScreenControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Control { source } => Some(source),
            Self::MissingState | Self::InvalidState | Self::EmptyState => None,
        }
    }
}

impl WebOsClient {
    pub fn turn_screen_off(&mut self) -> Result<WebOsPowerState, WebOsScreenControlError> {
        self.set_screen_power(TURN_OFF_SCREEN_URI)
    }

    pub fn turn_screen_on(&mut self) -> Result<WebOsPowerState, WebOsScreenControlError> {
        self.set_screen_power(TURN_ON_SCREEN_URI)
    }

    fn set_screen_power(&mut self, uri: &str) -> Result<WebOsPowerState, WebOsScreenControlError> {
        let payload = send_control_request(self, uri, json!({"standbyMode": "active"}))
            .map_err(|source| WebOsScreenControlError::Control { source })?;
        parse_screen_state(&payload)
    }
}

fn parse_screen_state(
    payload: &Map<String, Value>,
) -> Result<WebOsPowerState, WebOsScreenControlError> {
    let state = match payload.get("state") {
        Some(Value::String(state)) if state.trim().is_empty() => {
            return Err(WebOsScreenControlError::EmptyState)
        }
        Some(Value::String(state)) => state.clone(),
        Some(_) => return Err(WebOsScreenControlError::InvalidState),
        None => return Err(WebOsScreenControlError::MissingState),
    };

    Ok(WebOsPowerState::from_wire_value(state))
}

#[cfg(test)]
mod tests {
    use super::{parse_screen_state, WebOsScreenControlError};
    use crate::web_os::WebOsPowerState;
    use serde_json::{json, Map, Value};

    fn payload(value: Value) -> Map<String, Value> {
        value.as_object().expect("object payload").clone()
    }

    #[test]
    fn parses_observed_and_future_screen_states() {
        assert_eq!(
            parse_screen_state(&payload(json!({"state": "Screen Off"}))).expect("screen-off state"),
            WebOsPowerState::ScreenOff
        );
        assert_eq!(
            parse_screen_state(&payload(json!({"state": "Active"}))).expect("active state"),
            WebOsPowerState::Active
        );
        assert_eq!(
            parse_screen_state(&payload(json!({"state": "Future State"}))).expect("future state"),
            WebOsPowerState::Other("Future State".to_string())
        );
    }

    #[test]
    fn malformed_screen_states_are_typed_errors() {
        let cases = [
            (payload(json!({})), WebOsScreenControlError::MissingState),
            (
                payload(json!({"state": 7})),
                WebOsScreenControlError::InvalidState,
            ),
            (
                payload(json!({"state": "  "})),
                WebOsScreenControlError::EmptyState,
            ),
        ];

        for (payload, expected) in cases {
            let actual = parse_screen_state(&payload).expect_err("state should fail");
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
    }
}
