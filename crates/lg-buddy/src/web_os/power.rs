use super::control::send_control_request;
use super::{WebOsClient, WebOsClientError, WebOsControlError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

const GET_POWER_STATE_URI: &str = "ssap://com.webos.service.tvpower/power/getPowerState";
const POWER_OFF_URI: &str = "ssap://system/turnOff";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebOsPowerState {
    Active,
    ActiveStandby,
    ScreenOff,
    Suspend,
    PowerOff,
    Unknown,
    Other(String),
}

impl WebOsPowerState {
    pub(crate) fn from_wire_value(value: String) -> Self {
        match value.as_str() {
            "Active" => Self::Active,
            "Active Standby" => Self::ActiveStandby,
            "Screen Off" => Self::ScreenOff,
            "Suspend" => Self::Suspend,
            "Power Off" => Self::PowerOff,
            "Unknown" => Self::Unknown,
            _ => Self::Other(value),
        }
    }
}

impl fmt::Display for WebOsPowerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Active => "Active",
            Self::ActiveStandby => "Active Standby",
            Self::ScreenOff => "Screen Off",
            Self::Suspend => "Suspend",
            Self::PowerOff => "Power Off",
            Self::Unknown => "Unknown",
            Self::Other(value) => value,
        };
        write!(f, "{value}")
    }
}

#[derive(Debug)]
pub enum WebOsPowerStateError {
    Request { source: WebOsClientError },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected { message: Option<String> },
    MissingState,
    InvalidState,
    EmptyState,
}

impl fmt::Display for WebOsPowerStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => write!(f, "could not read webOS power state: {source}"),
            Self::MissingPayload => write!(f, "webOS power-state response has no payload"),
            Self::InvalidPayload => {
                write!(f, "webOS power-state response payload is not an object")
            }
            Self::MissingReturnValue => {
                write!(f, "webOS power-state response has no return value")
            }
            Self::InvalidReturnValue => {
                write!(
                    f,
                    "webOS power-state response return value is not a boolean"
                )
            }
            Self::RequestRejected {
                message: Some(message),
            } => write!(f, "webOS power-state request was rejected: {message}"),
            Self::RequestRejected { message: None } => {
                write!(f, "webOS power-state request was rejected")
            }
            Self::MissingState => write!(f, "webOS power-state response has no state"),
            Self::InvalidState => write!(f, "webOS power-state response state is not a string"),
            Self::EmptyState => write!(f, "webOS power-state response state is empty"),
        }
    }
}

impl Error for WebOsPowerStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. }
            | Self::MissingState
            | Self::InvalidState
            | Self::EmptyState => None,
        }
    }
}

impl WebOsClient {
    pub fn power_state(&mut self) -> Result<WebOsPowerState, WebOsPowerStateError> {
        let response = self
            .send_request(GET_POWER_STATE_URI, json!({}))
            .map_err(|source| WebOsPowerStateError::Request { source })?;
        parse_power_state_response(&response)
    }

    /// Powers off the TV and consumes the client so its connection cannot be reused.
    pub fn power_off(mut self) -> Result<(), WebOsControlError> {
        send_control_request(&mut self, POWER_OFF_URI, json!({})).map(|_| ())
    }
}

fn parse_power_state_response(response: &Value) -> Result<WebOsPowerState, WebOsPowerStateError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsPowerStateError::InvalidPayload),
        None => return Err(WebOsPowerStateError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(WebOsPowerStateError::RequestRejected { message });
        }
        Some(_) => return Err(WebOsPowerStateError::InvalidReturnValue),
        None => return Err(WebOsPowerStateError::MissingReturnValue),
    }

    let state = match payload.get("state") {
        Some(Value::String(state)) if state.trim().is_empty() => {
            return Err(WebOsPowerStateError::EmptyState)
        }
        Some(Value::String(state)) => state.clone(),
        Some(_) => return Err(WebOsPowerStateError::InvalidState),
        None => return Err(WebOsPowerStateError::MissingState),
    };

    Ok(WebOsPowerState::from_wire_value(state))
}

#[cfg(test)]
mod tests {
    use super::{parse_power_state_response, WebOsPowerState, WebOsPowerStateError};
    use serde_json::json;

    #[test]
    fn active_and_standby_states_parse_to_typed_values() {
        let cases = [
            ("Active", WebOsPowerState::Active),
            ("Active Standby", WebOsPowerState::ActiveStandby),
            ("Screen Off", WebOsPowerState::ScreenOff),
            ("Suspend", WebOsPowerState::Suspend),
        ];

        for (wire_value, expected) in cases {
            let response = json!({
                "payload": {"returnValue": true, "state": wire_value},
            });
            let actual = parse_power_state_response(&response).expect("valid power state");
            assert_eq!(actual, expected);
            assert_eq!(actual.to_string(), wire_value);
        }
    }

    #[test]
    fn unknown_future_state_is_preserved() {
        let response = json!({
            "payload": {"returnValue": true, "state": "Eco Standby"},
        });

        assert_eq!(
            parse_power_state_response(&response).expect("forward-compatible power state"),
            WebOsPowerState::Other("Eco Standby".to_string())
        );
    }

    #[test]
    fn malformed_power_state_payloads_are_typed_errors() {
        let cases = [
            (json!({}), "missing-payload"),
            (json!({"payload": []}), "invalid-payload"),
            (json!({"payload": {"state": "Active"}}), "missing-return"),
            (
                json!({"payload": {"returnValue": "yes", "state": "Active"}}),
                "invalid-return",
            ),
            (json!({"payload": {"returnValue": true}}), "missing-state"),
            (
                json!({"payload": {"returnValue": true, "state": 7}}),
                "invalid-state",
            ),
            (
                json!({"payload": {"returnValue": true, "state": " "}}),
                "empty-state",
            ),
        ];

        for (response, expected) in cases {
            let error = parse_power_state_response(&response)
                .expect_err("malformed power-state payload should fail");
            assert!(
                matches!(
                    (&error, expected),
                    (WebOsPowerStateError::MissingPayload, "missing-payload")
                        | (WebOsPowerStateError::InvalidPayload, "invalid-payload")
                        | (WebOsPowerStateError::MissingReturnValue, "missing-return")
                        | (WebOsPowerStateError::InvalidReturnValue, "invalid-return")
                        | (WebOsPowerStateError::MissingState, "missing-state")
                        | (WebOsPowerStateError::InvalidState, "invalid-state")
                        | (WebOsPowerStateError::EmptyState, "empty-state")
                ),
                "unexpected error for {expected}: {error}"
            );
        }
    }

    #[test]
    fn rejected_power_state_response_preserves_message() {
        let response = json!({
            "payload": {"returnValue": false, "errorText": "not available"},
        });

        assert!(matches!(
            parse_power_state_response(&response),
            Err(WebOsPowerStateError::RequestRejected {
                message: Some(message),
            }) if message == "not available"
        ));
    }
}
