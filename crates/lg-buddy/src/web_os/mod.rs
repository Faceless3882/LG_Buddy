//! Native LG webOS protocol support.

mod adapter;
mod client;
mod control;
mod input;
#[cfg(test)]
mod observed_behavior;
mod picture;
mod power;
mod registration;
mod screen;
#[cfg(test)]
mod test_support;
mod tls;

pub(crate) use adapter::WebOsPairingPolicy;
pub use adapter::WebOsTvClient;
pub(crate) use client::WebOsClientRegistration;
pub use client::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsClientError,
    WebOsClientRegistrationError, WebOsEndpoint,
};
pub use control::WebOsControlError;
pub use input::{WebOsForegroundApp, WebOsForegroundAppError, WebOsInputId, WebOsInputIdError};
pub use picture::{
    WebOsBacklightBrightness, WebOsBacklightBrightnessError, WebOsBacklightBrightnessValueError,
    WebOsSetBacklightBrightnessError,
};
pub use power::{WebOsPowerState, WebOsPowerStateError};
pub use screen::WebOsScreenControlError;

pub use registration::{
    parse_registration_message, WebOsRegistrationError, WebOsRegistrationEvent,
    WebOsRegistrationRequest,
};
