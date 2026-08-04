use super::control::send_control_request;
use super::{WebOsClient, WebOsClientError, WebOsControlError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

const GET_FOREGROUND_APP_URI: &str = "ssap://com.webos.applicationManager/getForegroundAppInfo";
const SWITCH_INPUT_URI: &str = "ssap://tv/switchInput";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebOsInputId(String);

impl WebOsInputId {
    pub fn new(value: impl Into<String>) -> Result<Self, WebOsInputIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(WebOsInputIdError::Empty);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WebOsInputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsInputIdError {
    Empty,
}

impl fmt::Display for WebOsInputIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "webOS input ID cannot be empty"),
        }
    }
}

impl Error for WebOsInputIdError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebOsForegroundApp {
    app_id: String,
}

impl WebOsForegroundApp {
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[cfg(test)]
    pub(crate) fn from_test_app_id(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
        }
    }
}

impl fmt::Display for WebOsForegroundApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.app_id)
    }
}

#[derive(Debug)]
pub enum WebOsForegroundAppError {
    Request { source: WebOsClientError },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected { message: Option<String> },
    MissingAppId,
    InvalidAppId,
    EmptyAppId,
}

impl fmt::Display for WebOsForegroundAppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => write!(f, "could not read webOS foreground app: {source}"),
            Self::MissingPayload => write!(f, "webOS foreground-app response has no payload"),
            Self::InvalidPayload => {
                write!(f, "webOS foreground-app response payload is not an object")
            }
            Self::MissingReturnValue => {
                write!(f, "webOS foreground-app response has no return value")
            }
            Self::InvalidReturnValue => {
                write!(
                    f,
                    "webOS foreground-app response return value is not a boolean"
                )
            }
            Self::RequestRejected {
                message: Some(message),
            } => write!(f, "webOS foreground-app request was rejected: {message}"),
            Self::RequestRejected { message: None } => {
                write!(f, "webOS foreground-app request was rejected")
            }
            Self::MissingAppId => write!(f, "webOS foreground-app response has no app ID"),
            Self::InvalidAppId => {
                write!(f, "webOS foreground-app response app ID is not a string")
            }
            Self::EmptyAppId => write!(f, "webOS foreground-app response app ID is empty"),
        }
    }
}

impl Error for WebOsForegroundAppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. }
            | Self::MissingAppId
            | Self::InvalidAppId
            | Self::EmptyAppId => None,
        }
    }
}

impl WebOsClient {
    pub fn foreground_app(&mut self) -> Result<WebOsForegroundApp, WebOsForegroundAppError> {
        let response = self
            .send_request(GET_FOREGROUND_APP_URI, json!({}))
            .map_err(|source| WebOsForegroundAppError::Request { source })?;
        parse_foreground_app_response(&response)
    }

    pub fn switch_input(&mut self, input_id: &WebOsInputId) -> Result<(), WebOsControlError> {
        send_control_request(
            self,
            SWITCH_INPUT_URI,
            json!({"inputId": input_id.as_str()}),
        )
        .map(|_| ())
    }
}

fn parse_foreground_app_response(
    response: &Value,
) -> Result<WebOsForegroundApp, WebOsForegroundAppError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsForegroundAppError::InvalidPayload),
        None => return Err(WebOsForegroundAppError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(WebOsForegroundAppError::RequestRejected { message });
        }
        Some(_) => return Err(WebOsForegroundAppError::InvalidReturnValue),
        None => return Err(WebOsForegroundAppError::MissingReturnValue),
    }

    let app_id = match payload.get("appId") {
        Some(Value::String(app_id)) if app_id.trim().is_empty() => {
            return Err(WebOsForegroundAppError::EmptyAppId)
        }
        Some(Value::String(app_id)) => app_id.clone(),
        Some(_) => return Err(WebOsForegroundAppError::InvalidAppId),
        None => return Err(WebOsForegroundAppError::MissingAppId),
    };

    Ok(WebOsForegroundApp { app_id })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_foreground_app_response, WebOsForegroundAppError, WebOsInputId, WebOsInputIdError,
    };
    use serde_json::json;

    #[test]
    fn input_id_rejects_empty_values() {
        assert_eq!(WebOsInputId::new(""), Err(WebOsInputIdError::Empty));
        assert_eq!(WebOsInputId::new("  "), Err(WebOsInputIdError::Empty));
        assert_eq!(
            WebOsInputId::new("HDMI_2").expect("input ID").as_str(),
            "HDMI_2"
        );
    }

    #[test]
    fn malformed_foreground_app_payloads_are_typed_errors() {
        let cases = [
            (json!({}), WebOsForegroundAppError::MissingPayload),
            (
                json!({"payload": []}),
                WebOsForegroundAppError::InvalidPayload,
            ),
            (
                json!({"payload": {"appId": "com.webos.app.hdmi3"}}),
                WebOsForegroundAppError::MissingReturnValue,
            ),
            (
                json!({"payload": {"returnValue": "yes", "appId": "com.webos.app.hdmi3"}}),
                WebOsForegroundAppError::InvalidReturnValue,
            ),
            (
                json!({"payload": {"returnValue": true}}),
                WebOsForegroundAppError::MissingAppId,
            ),
            (
                json!({"payload": {"returnValue": true, "appId": 3}}),
                WebOsForegroundAppError::InvalidAppId,
            ),
            (
                json!({"payload": {"returnValue": true, "appId": "  "}}),
                WebOsForegroundAppError::EmptyAppId,
            ),
        ];

        for (response, expected) in cases {
            let actual =
                parse_foreground_app_response(&response).expect_err("payload should be rejected");
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
    }

    #[test]
    fn rejected_foreground_app_response_preserves_message() {
        assert!(matches!(
            parse_foreground_app_response(&json!({
                "payload": {
                    "returnValue": false,
                    "errorText": "foreground app unavailable",
                },
            })),
            Err(WebOsForegroundAppError::RequestRejected {
                message: Some(message),
            }) if message == "foreground app unavailable"
        ));
    }
}
