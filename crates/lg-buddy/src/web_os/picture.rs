use super::control::send_control_request;
use super::{WebOsClient, WebOsClientError, WebOsControlError};
use serde_json::{json, Map, Value};
use std::error::Error;
use std::fmt;
use std::thread;
use std::time::Duration;

const GET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/getSystemSettings";
const SET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/setSystemSettings";
const SET_SYSTEM_SETTINGS_METHOD: &str = "setSystemSettings";
const MAX_BACKLIGHT_BRIGHTNESS: u8 = 100;
const BACKLIGHT_WRITE_READBACK_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsBacklightBrightness(u8);

impl WebOsBacklightBrightness {
    pub fn new(value: u8) -> Result<Self, WebOsBacklightBrightnessValueError> {
        if value <= MAX_BACKLIGHT_BRIGHTNESS {
            Ok(Self(value))
        } else {
            Err(WebOsBacklightBrightnessValueError { value })
        }
    }

    pub fn as_percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for WebOsBacklightBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsBacklightBrightnessValueError {
    value: u8,
}

impl fmt::Display for WebOsBacklightBrightnessValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "webOS backlight brightness `{}` is outside 0-{MAX_BACKLIGHT_BRIGHTNESS}",
            self.value
        )
    }
}

impl Error for WebOsBacklightBrightnessValueError {}

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
    InvalidBacklight { value: Value },
    BacklightOutOfRange { value: Value },
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
            Self::InvalidBacklight { value } => {
                write!(
                    f,
                    "webOS backlight-brightness response value `{value}` is not a whole number"
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
            | Self::InvalidBacklight { .. }
            | Self::BacklightOutOfRange { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum WebOsSetBacklightBrightnessError {
    Control {
        source: WebOsControlError,
    },
    MissingMethod,
    InvalidMethod,
    UnexpectedMethod {
        method: String,
    },
    Readback {
        expected: WebOsBacklightBrightness,
        source: WebOsBacklightBrightnessError,
    },
    NotApplied {
        expected: WebOsBacklightBrightness,
        actual: WebOsBacklightBrightness,
    },
}

impl fmt::Display for WebOsSetBacklightBrightnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control { source } => {
                write!(f, "could not set webOS backlight brightness: {source}")
            }
            Self::MissingMethod => {
                write!(f, "webOS backlight-write response has no method")
            }
            Self::InvalidMethod => {
                write!(f, "webOS backlight-write response method is not a string")
            }
            Self::UnexpectedMethod { method } => write!(
                f,
                "webOS backlight-write response method `{method}` is not `{SET_SYSTEM_SETTINGS_METHOD}`"
            ),
            Self::Readback { expected, source } => write!(
                f,
                "webOS acknowledged backlight brightness {expected}, but verification failed: {source}"
            ),
            Self::NotApplied { expected, actual } => write!(
                f,
                "webOS acknowledged backlight brightness {expected}, but readback was {actual}"
            ),
        }
    }
}

impl Error for WebOsSetBacklightBrightnessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Control { source } => Some(source),
            Self::Readback { source, .. } => Some(source),
            Self::MissingMethod
            | Self::InvalidMethod
            | Self::UnexpectedMethod { .. }
            | Self::NotApplied { .. } => None,
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

    pub fn set_backlight_brightness(
        &mut self,
        brightness: WebOsBacklightBrightness,
    ) -> Result<(), WebOsSetBacklightBrightnessError> {
        let payload = send_control_request(
            self,
            SET_SYSTEM_SETTINGS_URI,
            json!({
                "category": "picture",
                "settings": {"backlight": brightness.as_percent()},
            }),
        )
        .map_err(|source| WebOsSetBacklightBrightnessError::Control { source })?;
        validate_backlight_write_acknowledgement(&payload)?;

        thread::sleep(BACKLIGHT_WRITE_READBACK_DELAY);
        let actual = self.backlight_brightness().map_err(|source| {
            WebOsSetBacklightBrightnessError::Readback {
                expected: brightness,
                source,
            }
        })?;
        if actual != brightness {
            return Err(WebOsSetBacklightBrightnessError::NotApplied {
                expected: brightness,
                actual,
            });
        }

        Ok(())
    }
}

fn validate_backlight_write_acknowledgement(
    payload: &Map<String, Value>,
) -> Result<(), WebOsSetBacklightBrightnessError> {
    match payload.get("method") {
        Some(Value::String(method)) if method == SET_SYSTEM_SETTINGS_METHOD => Ok(()),
        Some(Value::String(method)) => Err(WebOsSetBacklightBrightnessError::UnexpectedMethod {
            method: method.clone(),
        }),
        Some(_) => Err(WebOsSetBacklightBrightnessError::InvalidMethod),
        None => Err(WebOsSetBacklightBrightnessError::MissingMethod),
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
        Some(value) => normalize_backlight_brightness(value)?,
        None => return Err(WebOsBacklightBrightnessError::MissingBacklight),
    };

    Ok(WebOsBacklightBrightness(value))
}

fn normalize_backlight_brightness(value: &Value) -> Result<u8, WebOsBacklightBrightnessError> {
    let normalized = match value {
        Value::Number(number) => {
            let Some(value_as_float) = number.as_f64() else {
                return Err(WebOsBacklightBrightnessError::InvalidBacklight {
                    value: value.clone(),
                });
            };
            if value_as_float.fract() != 0.0 {
                return Err(WebOsBacklightBrightnessError::InvalidBacklight {
                    value: value.clone(),
                });
            }
            value_as_float
        }
        Value::String(value_as_string) => value_as_string.parse::<u64>().map_or_else(
            |_| {
                Err(WebOsBacklightBrightnessError::InvalidBacklight {
                    value: value.clone(),
                })
            },
            |value_as_integer| Ok(value_as_integer as f64),
        )?,
        _ => {
            return Err(WebOsBacklightBrightnessError::InvalidBacklight {
                value: value.clone(),
            })
        }
    };
    if !(0.0..=f64::from(MAX_BACKLIGHT_BRIGHTNESS)).contains(&normalized) {
        return Err(WebOsBacklightBrightnessError::BacklightOutOfRange {
            value: value.clone(),
        });
    }

    Ok(normalized as u8)
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_backlight_brightness, parse_backlight_brightness_response,
        validate_backlight_write_acknowledgement, WebOsBacklightBrightness,
        WebOsBacklightBrightnessError, WebOsSetBacklightBrightnessError,
    };
    use serde_json::{json, Value};

    #[test]
    fn backlight_brightness_rejects_values_above_one_hundred() {
        assert_eq!(
            WebOsBacklightBrightness::new(0)
                .expect("minimum brightness")
                .as_percent(),
            0
        );
        assert_eq!(
            WebOsBacklightBrightness::new(100)
                .expect("maximum brightness")
                .as_percent(),
            100
        );
        assert!(WebOsBacklightBrightness::new(101).is_err());
    }

    #[test]
    fn observed_backlight_write_acknowledgement_is_accepted() {
        let payload = json!({"method": "setSystemSettings", "returnValue": true});
        validate_backlight_write_acknowledgement(payload.as_object().expect("object payload"))
            .expect("observed acknowledgement");
    }

    #[test]
    fn malformed_backlight_write_acknowledgements_are_typed_errors() {
        for (payload, expected) in [
            (json!({}), WebOsSetBacklightBrightnessError::MissingMethod),
            (
                json!({"method": 7}),
                WebOsSetBacklightBrightnessError::InvalidMethod,
            ),
            (
                json!({"method": "other"}),
                WebOsSetBacklightBrightnessError::UnexpectedMethod {
                    method: "other".to_string(),
                },
            ),
        ] {
            let actual = validate_backlight_write_acknowledgement(
                payload.as_object().expect("object payload"),
            )
            .expect_err("acknowledgement should be rejected");
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
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
                json!({"payload": {"returnValue": true, "settings": {"backlight": 101}}}),
                WebOsBacklightBrightnessError::BacklightOutOfRange { value: json!(101) },
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
    fn observed_integral_numeric_and_decimal_string_backlight_values_are_normalized() {
        for (wire_value, expected) in [
            (json!(0), 0),
            (json!(42), 42),
            (json!(42.0), 42),
            (json!("90"), 90),
            (json!(100.0), 100),
            (json!(1e2), 100),
        ] {
            assert_eq!(
                normalize_backlight_brightness(&wire_value)
                    .expect("whole numeric percentage should normalize"),
                expected
            );
        }
    }

    #[test]
    fn fractional_non_numeric_and_out_of_range_values_are_preserved_in_errors() {
        for wire_value in [json!(42.5), json!("42.0"), json!("bright"), Value::Null] {
            assert!(matches!(
                normalize_backlight_brightness(&wire_value),
                Err(WebOsBacklightBrightnessError::InvalidBacklight { value })
                    if value == wire_value
            ));
        }

        for wire_value in [
            json!(-1),
            json!(-1.0),
            json!(101),
            json!(101.0),
            json!("101"),
        ] {
            assert!(matches!(
                normalize_backlight_brightness(&wire_value),
                Err(WebOsBacklightBrightnessError::BacklightOutOfRange { value })
                    if value == wire_value
            ));
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
