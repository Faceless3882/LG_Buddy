use crate::platform_access_token::{PlatformAccessToken, PlatformAccessTokenError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

const REGISTRATION_MESSAGE_TYPE: &str = "register";
const PAIRING_PROMPT_TYPE: &str = "PROMPT";
// Keep the dev probe grant limited to its first read-only operation. Expanding
// permissions may invalidate stored client keys and require re-pairing.
const REGISTRATION_MANIFEST_JSON: &str = include_str!("registration_manifest.json");

static REGISTRATION_MANIFEST: OnceLock<Value> = OnceLock::new();

#[derive(Clone, PartialEq, Eq)]
pub struct WebOsRegistrationRequest {
    request_id: String,
    access_token: Option<PlatformAccessToken>,
}

impl WebOsRegistrationRequest {
    pub fn new(
        request_id: impl Into<String>,
        access_token: Option<&PlatformAccessToken>,
    ) -> Result<Self, WebOsRegistrationError> {
        let request_id = request_id.into();
        if request_id.trim().is_empty() {
            return Err(WebOsRegistrationError::EmptyRequestId);
        }

        Ok(Self {
            request_id,
            access_token: access_token.cloned(),
        })
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn to_json_value(&self) -> Value {
        let client_key = self
            .access_token
            .as_ref()
            .map(PlatformAccessToken::as_secret_str);

        json!({
            "type": REGISTRATION_MESSAGE_TYPE,
            "id": self.request_id,
            "payload": {
                "client-key": client_key,
                "forcePairing": false,
                "manifest": registration_manifest(),
                "pairingType": PAIRING_PROMPT_TYPE,
            },
        })
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.to_json_value())
            .expect("registration request contains only serializable JSON values")
    }
}

impl fmt::Debug for WebOsRegistrationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebOsRegistrationRequest")
            .field("request_id", &self.request_id)
            .field("has_access_token", &self.access_token.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebOsRegistrationEvent {
    PairingPrompt,
    Registered { access_token: PlatformAccessToken },
}

#[derive(Debug)]
pub enum WebOsRegistrationError {
    EmptyRequestId,
    MalformedJson(serde_json::Error),
    InvalidRootMessage,
    MissingRequestId,
    InvalidRequestId,
    UnexpectedRequestId { expected: String, actual: String },
    MissingMessageType,
    InvalidMessageType,
    MissingPayload { message_type: String },
    InvalidPayload { message_type: String },
    UnexpectedMessageType { message_type: String },
    UnexpectedRegistrationResponse,
    PairingRejected { message: Option<String> },
    MissingClientKey,
    InvalidClientKeyType,
    InvalidClientKey(PlatformAccessTokenError),
    MissingWebOsErrorMessage,
    InvalidWebOsErrorMessage,
    WebOsError { code: Option<i32>, message: String },
}

impl fmt::Display for WebOsRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRequestId => write!(f, "webOS registration request ID cannot be empty"),
            Self::MalformedJson(source) => {
                write!(f, "webOS registration response is malformed JSON: {source}")
            }
            Self::InvalidRootMessage => {
                write!(f, "webOS registration response root is not an object")
            }
            Self::MissingRequestId => write!(f, "webOS registration response has no request ID"),
            Self::InvalidRequestId => {
                write!(f, "webOS registration response request ID is not a string")
            }
            Self::UnexpectedRequestId { expected, actual } => write!(
                f,
                "webOS registration response ID `{actual}` does not match `{expected}`"
            ),
            Self::MissingMessageType => {
                write!(f, "webOS registration response has no message type")
            }
            Self::InvalidMessageType => {
                write!(
                    f,
                    "webOS registration response message type is not a string"
                )
            }
            Self::MissingPayload { message_type } => write!(
                f,
                "webOS `{message_type}` registration response has no payload"
            ),
            Self::InvalidPayload { message_type } => write!(
                f,
                "webOS `{message_type}` registration response payload is not an object"
            ),
            Self::UnexpectedMessageType { message_type } => {
                write!(
                    f,
                    "unexpected webOS registration message type `{message_type}`"
                )
            }
            Self::UnexpectedRegistrationResponse => {
                write!(f, "webOS registration response is not a pairing prompt")
            }
            Self::PairingRejected {
                message: Some(message),
            } => {
                write!(f, "webOS pairing was rejected: {message}")
            }
            Self::PairingRejected { message: None } => write!(f, "webOS pairing was rejected"),
            Self::MissingClientKey => {
                write!(f, "webOS registered response has no client key")
            }
            Self::InvalidClientKeyType => {
                write!(f, "webOS registered response client key is not a string")
            }
            Self::InvalidClientKey(source) => {
                write!(
                    f,
                    "webOS registered response has an invalid client key: {source}"
                )
            }
            Self::MissingWebOsErrorMessage => {
                write!(f, "webOS error response has no error message")
            }
            Self::InvalidWebOsErrorMessage => {
                write!(f, "webOS error response message is not a string")
            }
            Self::WebOsError {
                code: Some(code),
                message,
            } => write!(f, "webOS registration error {code}: {message}"),
            Self::WebOsError {
                code: None,
                message,
            } => write!(f, "webOS registration error: {message}"),
        }
    }
}

impl Error for WebOsRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MalformedJson(source) => Some(source),
            Self::InvalidClientKey(source) => Some(source),
            Self::EmptyRequestId
            | Self::InvalidRootMessage
            | Self::MissingRequestId
            | Self::InvalidRequestId
            | Self::UnexpectedRequestId { .. }
            | Self::MissingMessageType
            | Self::InvalidMessageType
            | Self::MissingPayload { .. }
            | Self::InvalidPayload { .. }
            | Self::UnexpectedMessageType { .. }
            | Self::UnexpectedRegistrationResponse
            | Self::PairingRejected { .. }
            | Self::MissingClientKey
            | Self::InvalidClientKeyType
            | Self::MissingWebOsErrorMessage
            | Self::InvalidWebOsErrorMessage
            | Self::WebOsError { .. } => None,
        }
    }
}

pub fn parse_registration_message(
    expected_request_id: &str,
    raw_message: &str,
) -> Result<WebOsRegistrationEvent, WebOsRegistrationError> {
    if expected_request_id.trim().is_empty() {
        return Err(WebOsRegistrationError::EmptyRequestId);
    }

    let message: Value =
        serde_json::from_str(raw_message).map_err(WebOsRegistrationError::MalformedJson)?;
    let message = message
        .as_object()
        .ok_or(WebOsRegistrationError::InvalidRootMessage)?;

    let actual_request_id = match message.get("id") {
        Some(Value::String(request_id)) => request_id,
        Some(_) => return Err(WebOsRegistrationError::InvalidRequestId),
        None => return Err(WebOsRegistrationError::MissingRequestId),
    };
    if actual_request_id != expected_request_id {
        return Err(WebOsRegistrationError::UnexpectedRequestId {
            expected: expected_request_id.to_string(),
            actual: actual_request_id.clone(),
        });
    }

    let message_type = match message.get("type") {
        Some(Value::String(message_type)) => message_type.as_str(),
        Some(_) => return Err(WebOsRegistrationError::InvalidMessageType),
        None => return Err(WebOsRegistrationError::MissingMessageType),
    };

    match message_type {
        "response" => parse_pairing_prompt(message, message_type),
        "registered" => parse_registered(message, message_type),
        "error" => Err(parse_webos_error(message)),
        other => Err(WebOsRegistrationError::UnexpectedMessageType {
            message_type: other.to_string(),
        }),
    }
}

fn parse_pairing_prompt(
    message: &serde_json::Map<String, Value>,
    message_type: &str,
) -> Result<WebOsRegistrationEvent, WebOsRegistrationError> {
    let payload = required_payload(message, message_type)?;

    if payload.get("returnValue").and_then(Value::as_bool) == Some(false) {
        let rejection_message = payload
            .get("errorText")
            .or_else(|| payload.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return Err(WebOsRegistrationError::PairingRejected {
            message: rejection_message,
        });
    }

    match payload.get("pairingType") {
        Some(Value::String(pairing_type)) if pairing_type == PAIRING_PROMPT_TYPE => {
            Ok(WebOsRegistrationEvent::PairingPrompt)
        }
        _ => Err(WebOsRegistrationError::UnexpectedRegistrationResponse),
    }
}

fn parse_registered(
    message: &serde_json::Map<String, Value>,
    message_type: &str,
) -> Result<WebOsRegistrationEvent, WebOsRegistrationError> {
    let payload = required_payload(message, message_type)?;
    let client_key = match payload.get("client-key") {
        Some(Value::String(client_key)) => client_key,
        Some(_) => return Err(WebOsRegistrationError::InvalidClientKeyType),
        None => return Err(WebOsRegistrationError::MissingClientKey),
    };
    let access_token = PlatformAccessToken::new(client_key.clone())
        .map_err(WebOsRegistrationError::InvalidClientKey)?;

    Ok(WebOsRegistrationEvent::Registered { access_token })
}

fn parse_webos_error(message: &serde_json::Map<String, Value>) -> WebOsRegistrationError {
    let error = match message.get("error") {
        Some(Value::String(error)) => error,
        Some(_) => return WebOsRegistrationError::InvalidWebOsErrorMessage,
        None => return WebOsRegistrationError::MissingWebOsErrorMessage,
    };
    let (code, message) = split_webos_error(error);
    WebOsRegistrationError::WebOsError { code, message }
}

fn required_payload<'a>(
    message: &'a serde_json::Map<String, Value>,
    message_type: &str,
) -> Result<&'a serde_json::Map<String, Value>, WebOsRegistrationError> {
    match message.get("payload") {
        Some(Value::Object(payload)) => Ok(payload),
        Some(_) => Err(WebOsRegistrationError::InvalidPayload {
            message_type: message_type.to_string(),
        }),
        None => Err(WebOsRegistrationError::MissingPayload {
            message_type: message_type.to_string(),
        }),
    }
}

fn split_webos_error(error: &str) -> (Option<i32>, String) {
    let mut parts = error.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or_default();
    match first.parse::<i32>() {
        Ok(code) => (
            Some(code),
            parts.next().unwrap_or_default().trim().to_string(),
        ),
        Err(_) => (None, error.to_string()),
    }
}

fn registration_manifest() -> &'static Value {
    REGISTRATION_MANIFEST.get_or_init(|| {
        serde_json::from_str(REGISTRATION_MANIFEST_JSON)
            .expect("bundled webOS registration manifest must be valid JSON")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_registration_message, registration_manifest, WebOsRegistrationError,
        WebOsRegistrationEvent, WebOsRegistrationRequest,
    };
    use crate::platform_access_token::{PlatformAccessToken, PlatformAccessTokenError};
    use serde_json::{json, Value};

    const REQUEST_ID: &str = "register_0";

    fn token(value: &str) -> PlatformAccessToken {
        PlatformAccessToken::new(value).expect("valid platform access token")
    }

    #[test]
    fn registration_request_without_token_uses_minimal_unsigned_probe_manifest() {
        let request =
            WebOsRegistrationRequest::new(REQUEST_ID, None).expect("registration request");
        let value = request.to_json_value();
        let expected_manifest = json!({
            "manifestVersion": 1,
            "permissions": ["READ_POWER_STATE"],
        });

        assert_eq!(value["type"], "register");
        assert_eq!(value["id"], REQUEST_ID);
        assert_eq!(value["payload"]["client-key"], Value::Null);
        assert_eq!(value["payload"]["forcePairing"], false);
        assert_eq!(value["payload"]["pairingType"], "PROMPT");
        assert_eq!(registration_manifest(), &expected_manifest);
        assert_eq!(value["payload"]["manifest"], expected_manifest);
        assert_eq!(
            serde_json::from_str::<Value>(&request.to_json_string())
                .expect("serialized registration request"),
            value
        );
    }

    #[test]
    fn registration_request_with_token_includes_key_without_leaking_it_in_debug() {
        let access_token = token("existing-client-key");
        let request = WebOsRegistrationRequest::new(REQUEST_ID, Some(&access_token))
            .expect("registration request");

        assert_eq!(request.request_id(), REQUEST_ID);
        assert_eq!(
            request.to_json_value()["payload"]["client-key"],
            access_token.as_secret_str()
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("has_access_token: true"));
        assert!(!debug.contains(access_token.as_secret_str()));
    }

    #[test]
    fn registration_request_rejects_empty_request_id() {
        assert!(matches!(
            WebOsRegistrationRequest::new(" ", None),
            Err(WebOsRegistrationError::EmptyRequestId)
        ));
    }

    #[test]
    fn prompt_then_registered_transcript_produces_token() {
        let prompt = json!({
            "id": REQUEST_ID,
            "type": "response",
            "payload": {
                "pairingType": "PROMPT",
                "returnValue": true,
            },
        });
        assert_eq!(
            parse_registration_message(REQUEST_ID, &prompt.to_string())
                .expect("parse pairing prompt"),
            WebOsRegistrationEvent::PairingPrompt
        );

        let registered = json!({
            "id": REQUEST_ID,
            "type": "registered",
            "payload": {"client-key": "new-client-key"},
        });
        assert_eq!(
            parse_registration_message(REQUEST_ID, &registered.to_string())
                .expect("parse registered response"),
            WebOsRegistrationEvent::Registered {
                access_token: token("new-client-key"),
            }
        );
    }

    #[test]
    fn response_id_is_required_and_must_match() {
        let cases = [
            (
                json!({"type": "registered", "payload": {"client-key": "key"}}),
                "missing",
            ),
            (
                json!({"id": 7, "type": "registered", "payload": {"client-key": "key"}}),
                "invalid",
            ),
            (
                json!({"id": "other", "type": "registered", "payload": {"client-key": "key"}}),
                "unexpected",
            ),
        ];

        for (message, expected) in cases {
            let error = parse_registration_message(REQUEST_ID, &message.to_string())
                .expect_err("request ID should fail validation");
            assert!(
                matches!(
                    (&error, expected),
                    (WebOsRegistrationError::MissingRequestId, "missing")
                        | (WebOsRegistrationError::InvalidRequestId, "invalid")
                        | (
                            WebOsRegistrationError::UnexpectedRequestId { .. },
                            "unexpected"
                        )
                ),
                "unexpected error for {expected}: {error}"
            );
        }
    }

    #[test]
    fn registered_response_requires_non_empty_string_client_key() {
        let cases = [
            (
                json!({"id": REQUEST_ID, "type": "registered", "payload": {}}),
                "missing",
            ),
            (
                json!({"id": REQUEST_ID, "type": "registered", "payload": {"client-key": 7}}),
                "invalid-type",
            ),
            (
                json!({"id": REQUEST_ID, "type": "registered", "payload": {"client-key": " "}}),
                "empty",
            ),
        ];

        for (message, expected) in cases {
            let error = parse_registration_message(REQUEST_ID, &message.to_string())
                .expect_err("invalid client key should fail");
            assert!(
                matches!(
                    (&error, expected),
                    (WebOsRegistrationError::MissingClientKey, "missing")
                        | (WebOsRegistrationError::InvalidClientKeyType, "invalid-type")
                        | (
                            WebOsRegistrationError::InvalidClientKey(
                                PlatformAccessTokenError::Empty
                            ),
                            "empty"
                        )
                ),
                "unexpected error for {expected}: {error}"
            );
        }
    }

    #[test]
    fn rejected_pairing_is_a_typed_error_with_preserved_message() {
        let response = json!({
            "id": REQUEST_ID,
            "type": "response",
            "payload": {
                "returnValue": false,
                "errorText": "user rejected pairing",
            },
        });

        assert!(matches!(
            parse_registration_message(REQUEST_ID, &response.to_string()),
            Err(WebOsRegistrationError::PairingRejected {
                message: Some(message)
            }) if message == "user rejected pairing"
        ));
    }

    #[test]
    fn webos_error_preserves_numeric_code_and_message() {
        let response = json!({
            "id": REQUEST_ID,
            "type": "error",
            "error": "-102 registration denied",
        });

        assert!(matches!(
            parse_registration_message(REQUEST_ID, &response.to_string()),
            Err(WebOsRegistrationError::WebOsError {
                code: Some(-102),
                message,
            }) if message == "registration denied"
        ));
    }

    #[test]
    fn malformed_and_unexpected_messages_are_typed_errors() {
        assert!(matches!(
            parse_registration_message(REQUEST_ID, "not-json"),
            Err(WebOsRegistrationError::MalformedJson(_))
        ));
        assert!(matches!(
            parse_registration_message(REQUEST_ID, "[]"),
            Err(WebOsRegistrationError::InvalidRootMessage)
        ));

        let cases = [
            (json!({"id": REQUEST_ID, "payload": {}}), "missing-type"),
            (
                json!({"id": REQUEST_ID, "type": 7, "payload": {}}),
                "invalid-type",
            ),
            (
                json!({"id": REQUEST_ID, "type": "hello", "payload": {}}),
                "unexpected-type",
            ),
            (
                json!({"id": REQUEST_ID, "type": "registered"}),
                "missing-payload",
            ),
            (
                json!({"id": REQUEST_ID, "type": "registered", "payload": 7}),
                "invalid-payload",
            ),
            (
                json!({"id": REQUEST_ID, "type": "response", "payload": {"returnValue": true}}),
                "unexpected-response",
            ),
            (json!({"id": REQUEST_ID, "type": "error"}), "missing-error"),
            (
                json!({"id": REQUEST_ID, "type": "error", "error": 7}),
                "invalid-error",
            ),
        ];

        for (message, expected) in cases {
            let error = parse_registration_message(REQUEST_ID, &message.to_string())
                .expect_err("malformed message should fail");
            assert!(
                matches!(
                    (&error, expected),
                    (WebOsRegistrationError::MissingMessageType, "missing-type")
                        | (WebOsRegistrationError::InvalidMessageType, "invalid-type")
                        | (
                            WebOsRegistrationError::UnexpectedMessageType { .. },
                            "unexpected-type"
                        )
                        | (
                            WebOsRegistrationError::MissingPayload { .. },
                            "missing-payload"
                        )
                        | (
                            WebOsRegistrationError::InvalidPayload { .. },
                            "invalid-payload"
                        )
                        | (
                            WebOsRegistrationError::UnexpectedRegistrationResponse,
                            "unexpected-response"
                        )
                        | (
                            WebOsRegistrationError::MissingWebOsErrorMessage,
                            "missing-error"
                        )
                        | (
                            WebOsRegistrationError::InvalidWebOsErrorMessage,
                            "invalid-error"
                        )
                ),
                "unexpected error for {expected}: {error}"
            );
        }
    }
}
