use super::control::send_control_request;
use super::{WebOsClient, WebOsClientError, WebOsControlError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

const GET_AUDIO_STATUS_URI: &str = "ssap://audio/getStatus";
const SET_AUDIO_VOLUME_URI: &str = "ssap://audio/setVolume";
const AUDIO_VOLUME_UP_URI: &str = "ssap://audio/volumeUp";
const AUDIO_VOLUME_DOWN_URI: &str = "ssap://audio/volumeDown";
const SET_AUDIO_MUTE_URI: &str = "ssap://audio/setMute";
const MAX_AUDIO_VOLUME: i64 = 100;

/// The volume reported by webOS. Some outputs report `-1` when no numeric
/// volume is available; that is a valid, explicit unknown state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsAudioVolume {
    Known(u8),
    Unknown,
}

impl fmt::Display for WebOsAudioVolume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(value) => write!(f, "{value}"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsAudioStatus {
    volume: WebOsAudioVolume,
    muted: bool,
}

impl WebOsAudioStatus {
    fn new(volume: WebOsAudioVolume, muted: bool) -> Self {
        Self { volume, muted }
    }

    pub fn volume(self) -> WebOsAudioVolume {
        self.volume
    }

    pub fn is_muted(self) -> bool {
        self.muted
    }
}

#[derive(Debug)]
pub enum WebOsAudioStatusError {
    Request { source: WebOsClientError },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected { message: Option<String> },
    MissingVolume,
    InvalidVolume { value: Value },
    MissingMute,
    InvalidMute,
}

impl fmt::Display for WebOsAudioStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => write!(f, "could not read webOS audio status: {source}"),
            Self::MissingPayload => write!(f, "webOS audio-status response has no payload"),
            Self::InvalidPayload => {
                write!(f, "webOS audio-status response payload is not an object")
            }
            Self::MissingReturnValue => {
                write!(f, "webOS audio-status response has no return value")
            }
            Self::InvalidReturnValue => {
                write!(
                    f,
                    "webOS audio-status response return value is not a boolean"
                )
            }
            Self::RequestRejected {
                message: Some(message),
            } => write!(f, "webOS audio-status request was rejected: {message}"),
            Self::RequestRejected { message: None } => {
                write!(f, "webOS audio-status request was rejected")
            }
            Self::MissingVolume => write!(f, "webOS audio-status response has no volume"),
            Self::InvalidVolume { value } => {
                write!(f, "webOS audio-status response volume `{value}` is invalid")
            }
            Self::MissingMute => write!(f, "webOS audio-status response has no mute state"),
            Self::InvalidMute => {
                write!(f, "webOS audio-status response mute state is not a boolean")
            }
        }
    }
}

impl Error for WebOsAudioStatusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. }
            | Self::MissingVolume
            | Self::InvalidVolume { .. }
            | Self::MissingMute
            | Self::InvalidMute => None,
        }
    }
}

impl WebOsClient {
    pub fn audio_status(&mut self) -> Result<WebOsAudioStatus, WebOsAudioStatusError> {
        let response = self
            .send_request(GET_AUDIO_STATUS_URI, json!({}))
            .map_err(|source| WebOsAudioStatusError::Request { source })?;
        parse_audio_status_response(&response)
    }

    /// Sets the absolute volume once. The caller owns any policy such as
    /// explicitly unmuting after this effectful operation.
    pub fn set_volume(&mut self, volume: u8) -> Result<(), WebOsControlError> {
        send_control_request(self, SET_AUDIO_VOLUME_URI, json!({"volume": volume})).map(|_| ())
    }

    pub fn volume_up(&mut self) -> Result<(), WebOsControlError> {
        send_control_request(self, AUDIO_VOLUME_UP_URI, json!({})).map(|_| ())
    }

    pub fn volume_down(&mut self) -> Result<(), WebOsControlError> {
        send_control_request(self, AUDIO_VOLUME_DOWN_URI, json!({})).map(|_| ())
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), WebOsControlError> {
        send_control_request(self, SET_AUDIO_MUTE_URI, json!({"mute": muted})).map(|_| ())
    }
}

fn parse_audio_status_response(
    response: &Value,
) -> Result<WebOsAudioStatus, WebOsAudioStatusError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsAudioStatusError::InvalidPayload),
        None => return Err(WebOsAudioStatusError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(WebOsAudioStatusError::RequestRejected { message });
        }
        Some(_) => return Err(WebOsAudioStatusError::InvalidReturnValue),
        None => return Err(WebOsAudioStatusError::MissingReturnValue),
    }

    let volume = match payload.get("volume") {
        Some(Value::Number(value)) => parse_audio_volume(value.as_i64().ok_or_else(|| {
            WebOsAudioStatusError::InvalidVolume {
                value: Value::Number(value.clone()),
            }
        })?)?,
        Some(value) => {
            return Err(WebOsAudioStatusError::InvalidVolume {
                value: value.clone(),
            })
        }
        None => return Err(WebOsAudioStatusError::MissingVolume),
    };
    let muted = match payload.get("mute") {
        Some(Value::Bool(muted)) => *muted,
        Some(_) => return Err(WebOsAudioStatusError::InvalidMute),
        None => return Err(WebOsAudioStatusError::MissingMute),
    };

    Ok(WebOsAudioStatus::new(volume, muted))
}

fn parse_audio_volume(value: i64) -> Result<WebOsAudioVolume, WebOsAudioStatusError> {
    match value {
        -1 => Ok(WebOsAudioVolume::Unknown),
        0..=MAX_AUDIO_VOLUME => Ok(WebOsAudioVolume::Known(value as u8)),
        _ => Err(WebOsAudioStatusError::InvalidVolume {
            value: json!(value),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_audio_status_response, WebOsAudioStatusError, WebOsAudioVolume};
    use serde_json::json;

    #[test]
    fn parses_observed_audio_status() {
        let response = json!({
            "payload": {
                "callerId": "com.webos.service.apiadapter",
                "mute": false,
                "returnValue": true,
                "volume": 20,
                "volumeStatus": {
                    "activeStatus": true,
                    "adjustVolume": true,
                    "externalDeviceControl": false,
                    "maxVolume": 100,
                    "mode": "normal",
                    "muteStatus": false,
                    "ossActivate": false,
                    "soundOutput": "headphone",
                    "volume": 20,
                    "volumeLimitable": true,
                    "volumeLimiter": "none",
                    "volumeSyncable": true,
                },
            },
        });

        let status = parse_audio_status_response(&response).expect("observed audio status");
        assert_eq!(status.volume(), WebOsAudioVolume::Known(20));
        assert!(!status.is_muted());
    }

    #[test]
    fn parses_minus_one_volume_as_unknown() {
        let response = json!({
            "payload": {"returnValue": true, "volume": -1, "mute": false},
        });

        let status = parse_audio_status_response(&response).expect("unknown audio volume");
        assert_eq!(status.volume(), WebOsAudioVolume::Unknown);
    }

    #[test]
    fn malformed_audio_status_payloads_are_typed_errors() {
        let cases = [
            (json!({}), "missing-payload"),
            (json!({"payload": []}), "invalid-payload"),
            (
                json!({"payload": {"volume": 20, "mute": false}}),
                "missing-return",
            ),
            (
                json!({"payload": {"returnValue": "yes", "volume": 20, "mute": false}}),
                "invalid-return",
            ),
            (
                json!({"payload": {"returnValue": true, "mute": false}}),
                "missing-volume",
            ),
            (
                json!({"payload": {"returnValue": true, "volume": 101, "mute": false}}),
                "invalid-volume",
            ),
            (
                json!({"payload": {"returnValue": true, "volume": 20}}),
                "missing-mute",
            ),
            (
                json!({"payload": {"returnValue": true, "volume": 20, "mute": "no"}}),
                "invalid-mute",
            ),
        ];

        for (response, expected) in cases {
            let error = parse_audio_status_response(&response).expect_err("status should fail");
            assert!(
                matches!(
                    (&error, expected),
                    (WebOsAudioStatusError::MissingPayload, "missing-payload")
                        | (WebOsAudioStatusError::InvalidPayload, "invalid-payload")
                        | (WebOsAudioStatusError::MissingReturnValue, "missing-return")
                        | (WebOsAudioStatusError::InvalidReturnValue, "invalid-return")
                        | (WebOsAudioStatusError::MissingVolume, "missing-volume")
                        | (
                            WebOsAudioStatusError::InvalidVolume { .. },
                            "invalid-volume"
                        )
                        | (WebOsAudioStatusError::MissingMute, "missing-mute")
                        | (WebOsAudioStatusError::InvalidMute, "invalid-mute")
                ),
                "unexpected error for {expected}: {error}"
            );
        }
    }
}
