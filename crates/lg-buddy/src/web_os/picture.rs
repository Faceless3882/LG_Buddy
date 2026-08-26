use super::control::send_control_request;
use super::{WebOsClient, WebOsClientError, WebOsControlError};
use serde_json::{json, Map, Value};
use std::error::Error;
use std::fmt;
use std::thread;
use std::time::Duration;

const GET_SYSTEM_SETTINGS_URI: &str = "ssap://settings/getSystemSettings";
const CREATE_ALERT_URI: &str = "ssap://system.notifications/createAlert";
const CLOSE_ALERT_URI: &str = "ssap://system.notifications/closeAlert";
const SET_SYSTEM_SETTINGS_LUNA_URI: &str = "luna://com.webos.settingsservice/setSystemSettings";
const ALERT_ID_RESPONSE_PREFIX: &str = "com.webos.service.apiadapter.pub-";
const ALERT_ID_CLOSE_PREFIX: &str = "com.webos.service.apiadapter-";
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
    CreateLunaBridge {
        source: WebOsControlError,
    },
    MissingAlertId,
    InvalidAlertId {
        value: Value,
    },
    UnexpectedAlertId {
        alert_id: String,
    },
    CloseLunaBridge {
        source: WebOsControlError,
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
            Self::CreateLunaBridge { source } => {
                write!(f, "could not create webOS Luna bridge alert: {source}")
            }
            Self::MissingAlertId => {
                write!(f, "webOS Luna bridge response has no alert ID")
            }
            Self::InvalidAlertId { value } => {
                write!(f, "webOS Luna bridge response alert ID `{value}` is not a string")
            }
            Self::UnexpectedAlertId { alert_id } => write!(
                f,
                "webOS Luna bridge returned unexpected alert ID `{alert_id}`"
            ),
            Self::CloseLunaBridge { source } => {
                write!(f, "could not close webOS Luna bridge alert: {source}")
            }
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
            Self::CreateLunaBridge { source } | Self::CloseLunaBridge { source } => Some(source),
            Self::Readback { source, .. } => Some(source),
            Self::MissingAlertId
            | Self::InvalidAlertId { .. }
            | Self::UnexpectedAlertId { .. }
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
        set_system_settings_via_luna_bridge(
            self,
            json!({
                "category": "picture",
                "settings": {"backlight": brightness.as_percent().to_string()},
            }),
        )?;

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

fn set_system_settings_via_luna_bridge(
    client: &mut WebOsClient,
    params: Value,
) -> Result<(), WebOsSetBacklightBrightnessError> {
    let callback = json!({
        "params": params.clone(),
        "uri": SET_SYSTEM_SETTINGS_LUNA_URI,
    });
    let payload = send_control_request(
        client,
        CREATE_ALERT_URI,
        json!({
            "buttons": [{
                "label": "",
                "onClick": SET_SYSTEM_SETTINGS_LUNA_URI,
                "params": params,
            }],
            "message": " ",
            "onclose": callback.clone(),
            "onfail": callback,
        }),
    )
    .map_err(|source| WebOsSetBacklightBrightnessError::CreateLunaBridge { source })?;
    let close_alert_id = close_alert_id(&payload)?;

    send_control_request(client, CLOSE_ALERT_URI, json!({"alertId": close_alert_id}))
        .map_err(|source| WebOsSetBacklightBrightnessError::CloseLunaBridge { source })?;
    Ok(())
}

fn close_alert_id(
    payload: &Map<String, Value>,
) -> Result<String, WebOsSetBacklightBrightnessError> {
    let alert_id = match payload.get("alertId") {
        Some(Value::String(alert_id)) => alert_id,
        Some(value) => {
            return Err(WebOsSetBacklightBrightnessError::InvalidAlertId {
                value: value.clone(),
            })
        }
        None => return Err(WebOsSetBacklightBrightnessError::MissingAlertId),
    };
    let sequence = alert_id
        .strip_prefix(ALERT_ID_RESPONSE_PREFIX)
        .filter(|sequence| !sequence.is_empty())
        .ok_or_else(|| WebOsSetBacklightBrightnessError::UnexpectedAlertId {
            alert_id: alert_id.clone(),
        })?;
    Ok(format!("{ALERT_ID_CLOSE_PREFIX}{sequence}"))
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
        close_alert_id, normalize_backlight_brightness, parse_backlight_brightness_response,
        WebOsBacklightBrightness, WebOsBacklightBrightnessError, WebOsSetBacklightBrightnessError,
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
    fn observed_luna_bridge_alert_id_maps_to_close_request_id() {
        let payload = json!({
            "returnValue": true,
            "alertId": "com.webos.service.apiadapter.pub-1786895965205",
        });
        assert_eq!(
            close_alert_id(payload.as_object().expect("object payload"))
                .expect("observed alert ID"),
            "com.webos.service.apiadapter-1786895965205"
        );
    }

    #[test]
    fn malformed_luna_bridge_alert_ids_are_typed_errors() {
        for (payload, expected) in [
            (json!({}), WebOsSetBacklightBrightnessError::MissingAlertId),
            (
                json!({"alertId": 7}),
                WebOsSetBacklightBrightnessError::InvalidAlertId { value: json!(7) },
            ),
            (
                json!({"alertId": "other"}),
                WebOsSetBacklightBrightnessError::UnexpectedAlertId {
                    alert_id: "other".to_string(),
                },
            ),
            (
                json!({"alertId": "com.webos.service.apiadapter.pub-"}),
                WebOsSetBacklightBrightnessError::UnexpectedAlertId {
                    alert_id: "com.webos.service.apiadapter.pub-".to_string(),
                },
            ),
        ] {
            let actual = close_alert_id(payload.as_object().expect("object payload"))
                .expect_err("alert ID should be rejected");
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
