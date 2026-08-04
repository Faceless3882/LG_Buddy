use super::{WebOsClient, WebOsClientError};
use serde_json::{Map, Value};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum WebOsControlError {
    Request {
        source: WebOsClientError,
    },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected {
        message: Option<String>,
        payload: Value,
    },
}

impl fmt::Display for WebOsControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => write!(f, "webOS control request failed: {source}"),
            Self::MissingPayload => write!(f, "webOS control response has no payload"),
            Self::InvalidPayload => write!(f, "webOS control response payload is not an object"),
            Self::MissingReturnValue => write!(f, "webOS control response has no return value"),
            Self::InvalidReturnValue => {
                write!(f, "webOS control response return value is not a boolean")
            }
            Self::RequestRejected {
                message: Some(message),
                ..
            } => write!(f, "webOS control request was rejected: {message}"),
            Self::RequestRejected { message: None, .. } => {
                write!(f, "webOS control request was rejected")
            }
        }
    }
}

impl Error for WebOsControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. } => None,
        }
    }
}

pub(crate) fn send_control_request(
    client: &mut WebOsClient,
    uri: &str,
    payload: Value,
) -> Result<Map<String, Value>, WebOsControlError> {
    let response = client
        .send_request(uri, payload)
        .map_err(|source| WebOsControlError::Request { source })?;
    parse_control_response(&response)
}

fn parse_control_response(response: &Value) -> Result<Map<String, Value>, WebOsControlError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsControlError::InvalidPayload),
        None => return Err(WebOsControlError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => Ok(payload.clone()),
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Err(WebOsControlError::RequestRejected {
                message,
                payload: Value::Object(payload.clone()),
            })
        }
        Some(_) => Err(WebOsControlError::InvalidReturnValue),
        None => Err(WebOsControlError::MissingReturnValue),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_control_response, WebOsControlError};
    use serde_json::json;

    #[test]
    fn accepts_successful_control_response() {
        assert!(parse_control_response(&json!({
            "payload": {"returnValue": true},
        }))
        .is_ok());
    }

    #[test]
    fn malformed_control_payloads_are_typed_errors() {
        let cases = [
            (json!({}), WebOsControlError::MissingPayload),
            (json!({"payload": []}), WebOsControlError::InvalidPayload),
            (
                json!({"payload": {}}),
                WebOsControlError::MissingReturnValue,
            ),
            (
                json!({"payload": {"returnValue": "yes"}}),
                WebOsControlError::InvalidReturnValue,
            ),
        ];

        for (response, expected) in cases {
            let actual = parse_control_response(&response).expect_err("response should fail");
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
        }
    }

    #[test]
    fn rejected_control_response_preserves_payload() {
        assert!(matches!(
            parse_control_response(&json!({
                "payload": {
                    "returnValue": false,
                    "errorCode": "-102",
                    "errorText": "wrong state",
                },
            })),
            Err(WebOsControlError::RequestRejected {
                message: Some(message),
                payload,
            }) if message == "wrong state"
                && payload == json!({
                    "returnValue": false,
                    "errorCode": "-102",
                    "errorText": "wrong state",
                })
        ));
    }
}
