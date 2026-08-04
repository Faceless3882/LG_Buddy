use super::test_support::ScriptedWebOsServer;
use super::{WebOsClient, WebOsClientError, WebOsInputId};
use serde_json::{json, Value};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(200);

const SET_INPUT_URI: &str = "ssap://tv/switchInput";
const TURN_OFF_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";
const TURN_ON_SCREEN_URI: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";
const TURN_OFF_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOffScreen";
const TURN_ON_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOnScreen";
const POWER_OFF_URI: &str = "ssap://system/turnOff";

fn connected_client(server: &ScriptedWebOsServer) -> WebOsClient {
    WebOsClient::connect_for_test(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
        .expect("connect characterized webOS client")
}

fn assert_request(request: &Value, uri: &str, payload: Value) {
    assert_eq!(request["id"], "request_0");
    assert_eq!(request["type"], "request");
    assert_eq!(request["uri"], uri);
    assert_eq!(request["payload"], payload);
}

// Hardware evidence:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176459681
#[test]
fn hardware_observed_input_switch_transcript_is_replayed_exactly() {
    let server = ScriptedWebOsServer::spawn(|peer| {
        let request = peer.receive_json();
        assert_request(&request, SET_INPUT_URI, json!({"inputId": "HDMI_2"}));
        peer.send_json(json!({
            "id": request["id"],
            "type": "response",
            "payload": {"returnValue": true},
        }));
    });
    let mut client = connected_client(&server);

    let input_id = WebOsInputId::new("HDMI_2").expect("input ID");
    client.switch_input(&input_id).expect("switch input");
    server.finish();
}

// Hardware evidence:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176461983
#[test]
fn hardware_observed_screen_off_transcript_is_replayed_exactly() {
    let server = ScriptedWebOsServer::spawn(|peer| {
        let request = peer.receive_json();
        assert_request(
            &request,
            TURN_OFF_SCREEN_URI,
            json!({"standbyMode": "active"}),
        );
        peer.send_json(json!({
            "id": request["id"],
            "type": "response",
            "payload": {"returnValue": true, "state": "Screen Off"},
        }));
    });
    let mut client = connected_client(&server);

    assert_eq!(
        client.turn_screen_off().expect("turn screen off"),
        super::WebOsPowerState::ScreenOff
    );
    server.finish();
}

#[test]
fn hardware_observed_screen_on_transcript_is_replayed_exactly() {
    let server = ScriptedWebOsServer::spawn(|peer| {
        let request = peer.receive_json();
        assert_request(
            &request,
            TURN_ON_SCREEN_URI,
            json!({"standbyMode": "active"}),
        );
        peer.send_json(json!({
            "id": request["id"],
            "type": "response",
            "payload": {"returnValue": true, "state": "Active"},
        }));
    });
    let mut client = connected_client(&server);

    assert_eq!(
        client.turn_screen_on().expect("turn screen on"),
        super::WebOsPowerState::Active
    );
    server.finish();
}

#[test]
fn hardware_observed_screen_on_substate_error_preserves_payload() {
    let server = ScriptedWebOsServer::spawn(|peer| {
        let request = peer.receive_json();
        assert_request(
            &request,
            TURN_ON_SCREEN_URI,
            json!({"standbyMode": "active"}),
        );
        peer.send_json(json!({
            "id": request["id"],
            "type": "error",
            "error": "500 Application error",
            "payload": {
                "errorCode": "-102",
                "errorText": "The current sub state must be 'screen off'",
                "returnValue": false,
                "state": "Active",
            },
        }));
    });
    let mut client = connected_client(&server);

    assert!(matches!(
        client.turn_screen_on(),
        Err(super::WebOsScreenControlError::Control {
            source: super::WebOsControlError::Request {
                source: WebOsClientError::WebOs {
                    code: Some(500),
                    message,
                    payload: Some(payload),
                },
            },
        }) if message == "Application error"
            && payload == json!({
                "errorCode": "-102",
                "errorText": "The current sub state must be 'screen off'",
                "returnValue": false,
                "state": "Active",
            })
    ));
    server.finish();
}

#[test]
fn hardware_observed_legacy_screen_endpoints_are_not_available() {
    for uri in [TURN_OFF_SCREEN_LEGACY_URI, TURN_ON_SCREEN_LEGACY_URI] {
        let server = ScriptedWebOsServer::spawn(move |peer| {
            let request = peer.receive_json();
            assert_request(&request, uri, json!({"standbyMode": "active"}));
            peer.send_json(json!({
                "id": request["id"],
                "type": "error",
                "error": "404 no such service or method",
                "payload": {},
            }));
        });
        let mut client = connected_client(&server);

        assert!(matches!(
            client.send_request(uri, json!({"standbyMode": "active"})),
            Err(WebOsClientError::WebOs {
                code: Some(404),
                message,
                payload: Some(payload),
            }) if message == "no such service or method" && payload == json!({})
        ));
        server.finish();
    }
}

// Hardware evidence:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176463406
#[test]
fn hardware_observed_power_off_transcript_is_replayed_exactly() {
    let server = ScriptedWebOsServer::spawn(|peer| {
        let request = peer.receive_json();
        assert_request(&request, POWER_OFF_URI, json!({}));
        peer.send_json(json!({
            "id": request["id"],
            "type": "response",
            "payload": {"returnValue": true},
        }));
    });
    let mut client = connected_client(&server);

    assert_eq!(
        client
            .send_request(POWER_OFF_URI, json!({}))
            .expect("power off response"),
        json!({
            "id": "request_0",
            "type": "response",
            "payload": {"returnValue": true},
        })
    );
    server.finish();
}
