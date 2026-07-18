//! Native LG webOS protocol support.

mod client;
mod registration;
mod tls;

pub(crate) use client::WebOsClientRegistration;
pub use client::{
    WebOsAuthenticatedClientError, WebOsClient, WebOsClientError, WebOsClientRegistrationError,
    WebOsEndpoint,
};

pub use registration::{
    parse_registration_message, WebOsRegistrationError, WebOsRegistrationEvent,
    WebOsRegistrationRequest,
};
