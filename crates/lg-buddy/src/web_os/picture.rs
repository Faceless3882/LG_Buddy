use super::{WebOsClient, WebOsClientError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

const GET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/getSystemSettings";
const MAX_BACKLIGHT_BRIGHTNESS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsBacklightBrightness(u8);

impl WebOsBacklightBrightness {
    pub fn as_percent(self) -> u8 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test_percent(value: u8) -> Self {
        Self(value)
    }
}

impl fmt::Display for WebOsBacklightBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug)]
pub enum WebOsBacklightBrightnessError {
    Request { source: WebOsClientError },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected { message: Option<String> },
    MissingSettings,
    InvalidSettings,
    MissingBacklight,
    InvalidBacklight,
    BacklightOutOfRange { value: u64 },
}

impl fmt::Display for WebOsBacklightBrightnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => {
                write!(f, "could not read webOS backlight brightness: {source}")
            }
            Self::MissingPayload => {
                write!(f, "webOS backlight-brightness response has no payload")
            }
            Self::InvalidPayload => {
                write!(
                    f,
                    "webOS backlight-brightness response payload is not an object"
                )
            }
            Self::MissingReturnValue => {
                write!(f, "webOS backlight-brightness response has no return value")
            }
            Self::InvalidReturnValue => {
                write!(
                    f,
                    "webOS backlight-brightness response return value is not a boolean"
                )
            }
            Self::RequestRejected {
                message: Some(message),
            } => write!(
                f,
                "webOS backlight-brightness request was rejected: {message}"
            ),
            Self::RequestRejected { message: None } => {
                write!(f, "webOS backlight-brightness request was rejected")
            }
            Self::MissingSettings => {
                write!(f, "webOS backlight-brightness response has no settings")
            }
            Self::InvalidSettings => {
                write!(
                    f,
                    "webOS backlight-brightness response settings are not an object"
                )
            }
            Self::MissingBacklight => {
                write!(
                    f,
                    "webOS backlight-brightness response has no backlight value"
                )
            }
            Self::InvalidBacklight => {
                write!(
                    f,
                    "webOS backlight-brightness response backlight value is not an integer"
                )
            }
            Self::BacklightOutOfRange { value } => write!(
                f,
                "webOS backlight-brightness response value `{value}` is outside 0-100"
            ),
        }
    }
}

impl Error for WebOsBacklightBrightnessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. }
            | Self::MissingSettings
            | Self::InvalidSettings
            | Self::MissingBacklight
            | Self::InvalidBacklight
            | Self::BacklightOutOfRange { .. } => None,
        }
    }
}

impl WebOsClient {
    pub fn backlight_brightness(
        &mut self,
    ) -> Result<WebOsBacklightBrightness, WebOsBacklightBrightnessError> {
        let response = self
            .send_request(
                GET_SYSTEM_SETTINGS_URI,
                json!({
                    "category": "picture",
                    "keys": ["backlight"],
                }),
            )
            .map_err(|source| WebOsBacklightBrightnessError::Request { source })?;
        parse_backlight_brightness_response(&response)
    }
}

fn parse_backlight_brightness_response(
    response: &Value,
) -> Result<WebOsBacklightBrightness, WebOsBacklightBrightnessError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsBacklightBrightnessError::InvalidPayload),
        None => return Err(WebOsBacklightBrightnessError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(WebOsBacklightBrightnessError::RequestRejected { message });
        }
        Some(_) => return Err(WebOsBacklightBrightnessError::InvalidReturnValue),
        None => return Err(WebOsBacklightBrightnessError::MissingReturnValue),
    }

    let settings = match payload.get("settings") {
        Some(Value::Object(settings)) => settings,
        Some(_) => return Err(WebOsBacklightBrightnessError::InvalidSettings),
        None => return Err(WebOsBacklightBrightnessError::MissingSettings),
    };
    let value = match settings.get("backlight") {
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or(WebOsBacklightBrightnessError::InvalidBacklight)?,
        Some(_) => return Err(WebOsBacklightBrightnessError::InvalidBacklight),
        None => return Err(WebOsBacklightBrightnessError::MissingBacklight),
    };
    if value > MAX_BACKLIGHT_BRIGHTNESS {
        return Err(WebOsBacklightBrightnessError::BacklightOutOfRange { value });
    }

    Ok(WebOsBacklightBrightness(value as u8))
}

#[cfg(test)]
mod tests {
    use super::{
        parse_backlight_brightness_response, WebOsBacklightBrightness,
        WebOsBacklightBrightnessError, GET_SYSTEM_SETTINGS_URI,
    };
    use crate::web_os::test_support::ScriptedWebOsServer;
    use crate::web_os::WebOsClient;
    use serde_json::json;
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RESPONSE_TIMEOUT: Duration = Duration::from_millis(200);

    #[test]
    fn backlight_request_matches_hardware_observed_transcript() {
        let server = ScriptedWebOsServer::spawn(|peer| {
            let request = peer.receive_json();
            assert_eq!(request["id"], "request_0");
            assert_eq!(request["type"], "request");
            assert_eq!(request["uri"], GET_SYSTEM_SETTINGS_URI);
            assert_eq!(
                request["payload"],
                json!({
                    "category": "picture",
                    "keys": ["backlight"],
                })
            );
            peer.send_json(json!({
                "id": request["id"],
                "type": "response",
                "payload": {
                    "category": "picture",
                    "returnValue": true,
                    "settings": {
                        "backlight": 100,
                    },
                    "subscribed": false,
                },
            }));
        });
        let mut client =
            WebOsClient::connect_for_test(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
                .expect("connect client");

        assert_eq!(
            client
                .backlight_brightness()
                .expect("read backlight brightness"),
            WebOsBacklightBrightness(100)
        );
        server.finish();
    }

    #[test]
    fn malformed_backlight_payloads_are_typed_errors() {
        let cases = [
            (json!({}), WebOsBacklightBrightnessError::MissingPayload),
            (
                json!({"payload": []}),
                WebOsBacklightBrightnessError::InvalidPayload,
            ),
            (
                json!({"payload": {"settings": {"backlight": 50}}}),
                WebOsBacklightBrightnessError::MissingReturnValue,
            ),
            (
                json!({"payload": {"returnValue": "yes", "settings": {"backlight": 50}}}),
                WebOsBacklightBrightnessError::InvalidReturnValue,
            ),
            (
                json!({"payload": {"returnValue": true}}),
                WebOsBacklightBrightnessError::MissingSettings,
            ),
            (
                json!({"payload": {"returnValue": true, "settings": []}}),
                WebOsBacklightBrightnessError::InvalidSettings,
            ),
            (
                json!({"payload": {"returnValue": true, "settings": {}}}),
                WebOsBacklightBrightnessError::MissingBacklight,
            ),
            (
                json!({"payload": {"returnValue": true, "settings": {"backlight": "50"}}}),
                WebOsBacklightBrightnessError::InvalidBacklight,
            ),
            (
                json!({"payload": {"returnValue": true, "settings": {"backlight": 101}}}),
                WebOsBacklightBrightnessError::BacklightOutOfRange { value: 101 },
            ),
        ];

        for (response, expected) in cases {
            let actual = parse_backlight_brightness_response(&response)
                .expect_err("payload should be rejected");
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
    }

    #[test]
    fn rejected_backlight_response_preserves_message() {
        assert!(matches!(
            parse_backlight_brightness_response(&json!({
                "payload": {
                    "returnValue": false,
                    "errorText": "settings unavailable",
                },
            })),
            Err(WebOsBacklightBrightnessError::RequestRejected {
                message: Some(message),
            }) if message == "settings unavailable"
        ));
    }
}
