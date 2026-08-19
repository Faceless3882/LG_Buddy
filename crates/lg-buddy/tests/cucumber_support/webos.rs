pub use lg_buddy::web_os::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsEndpoint,
    WebOsPowerState,
};
use serde_json::Value;
use std::net::{Ipv4Addr, SocketAddr};

mod test_support;

use test_support::test_server::{
    WebOsTestInput, WebOsTestScenario, WebOsTestServer, WebOsTestTvSnapshot,
};

pub const VALID_WEBOS_ACCESS_TOKEN: &str = "webos-test-access-token";
const WEBOS_WSS_PORT: u16 = 3001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockWebOsTvSnapshot {
    pub power_on: bool,
    pub screen_on: bool,
    pub input: String,
    pub backlight: u8,
    pub connection_count: u64,
    pub pairing_prompt_count: u64,
    pub registration_tokens: Vec<Option<String>>,
}

pub struct MockWebOsTv {
    server: WebOsTestServer,
}

impl MockWebOsTv {
    pub fn new(input: &str) -> Self {
        let input = match input {
            "HDMI_2" => WebOsTestInput::Hdmi2,
            "HDMI_3" => WebOsTestInput::Hdmi3,
            other => panic!("no hardware-backed native WebOS fixture exists for `{other}`"),
        };
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, WEBOS_WSS_PORT));
        Self {
            server: WebOsTestServer::active_tls_at(input, address),
        }
    }

    pub fn reject_pairing(&self) {
        self.server.set_scenario(WebOsTestScenario::PairingRejected);
    }

    pub fn require_stale_token_pairing(&self) {
        self.server
            .set_scenario(WebOsTestScenario::StoredTokenPairingPrompt);
    }

    pub fn snapshot(&self) -> MockWebOsTvSnapshot {
        self.assert_healthy();
        let snapshot = self.server.snapshot();
        MockWebOsTvSnapshot {
            power_on: snapshot.power_state != WebOsPowerState::PowerOff,
            screen_on: snapshot.power_state == WebOsPowerState::Active,
            input: input_name(snapshot.input).to_string(),
            backlight: backlight_value(&snapshot),
            connection_count: snapshot.connection_count,
            pairing_prompt_count: snapshot.pairing_prompt_count,
            registration_tokens: snapshot.registration_tokens,
        }
    }

    pub fn assert_healthy(&self) {
        self.server.assert_healthy();
    }
}

fn input_name(input: WebOsTestInput) -> &'static str {
    match input {
        WebOsTestInput::Hdmi2 => "HDMI_2",
        WebOsTestInput::Hdmi3 => "HDMI_3",
    }
}

fn backlight_value(snapshot: &WebOsTestTvSnapshot) -> u8 {
    match &snapshot.backlight {
        Value::Number(value) => value.as_u64().expect("native backlight integer") as u8,
        Value::String(value) => value.parse().expect("native backlight numeric string"),
        other => panic!("unexpected native backlight value: {other}"),
    }
}
