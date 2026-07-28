//! Native LG webOS protocol support.

mod client;
mod input;
mod picture;
mod power;
mod registration;
#[cfg(test)]
mod test_support;
mod tls;

pub(crate) use client::WebOsClientRegistration;
pub use client::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsClientError,
    WebOsClientRegistrationError, WebOsEndpoint,
};
pub use input::{WebOsForegroundApp, WebOsForegroundAppError};
pub use picture::{WebOsBacklightBrightness, WebOsBacklightBrightnessError};
pub use power::{WebOsPowerState, WebOsPowerStateError};

pub use registration::{
    parse_registration_message, WebOsRegistrationError, WebOsRegistrationEvent,
    WebOsRegistrationRequest,
};
