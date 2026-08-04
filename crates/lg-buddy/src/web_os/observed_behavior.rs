use super::test_support::{ObservedWebOsInput, ObservedWebOsTvServer};
use super::{
    WebOsAuthenticatedClientError, WebOsClientError, WebOsClientRegistrationError,
    WebOsControlError, WebOsInputId, WebOsPowerState, WebOsScreenControlError,
};
use crate::platform_access_token::PlatformAccessTokenAcquisitionError;
use serde_json::json;

const TURN_OFF_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOffScreen";
const TURN_ON_SCREEN_LEGACY_URI: &str = "ssap://com.webos.service.tv.power/turnOnScreen";

// Real-TV wire observations:
// https://github.com/Staphylococcus/LG_Buddy/issues/50#issuecomment-5102465348
// https://github.com/Staphylococcus/LG_Buddy/issues/50#issuecomment-5102531370
#[test]
fn read_operations_return_the_observed_active_tv_state() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert_eq!(
        client.power_state().expect("read power state"),
        WebOsPowerState::Active
    );
    assert_eq!(
        client
            .foreground_app()
            .expect("read foreground app")
            .app_id(),
        "com.webos.app.hdmi3"
    );
    assert_eq!(
        client
            .backlight_brightness()
            .expect("read backlight brightness")
            .as_percent(),
        100
    );

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176459681
// https://github.com/Staphylococcus/LG_Buddy/issues/50#issuecomment-5179994257
#[test]
fn input_switch_changes_the_observed_foreground_app_and_picture_state() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert_eq!(
        client
            .foreground_app()
            .expect("read initial foreground app")
            .app_id(),
        "com.webos.app.hdmi3"
    );
    client
        .switch_input(&WebOsInputId::new("HDMI_2").expect("input ID"))
        .expect("switch input");
    assert_eq!(
        client
            .foreground_app()
            .expect("read resulting foreground app")
            .app_id(),
        "com.webos.app.hdmi2"
    );
    assert_eq!(
        client
            .backlight_brightness()
            .expect("read resulting backlight brightness")
            .as_percent(),
        90
    );
    assert_eq!(server.snapshot().input, ObservedWebOsInput::Hdmi2);

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176461983
#[test]
fn screen_off_transitions_active_tv_to_screen_off() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert_eq!(
        client.power_state().expect("read initial power state"),
        WebOsPowerState::Active
    );
    assert_eq!(
        client.turn_screen_off().expect("turn screen off"),
        WebOsPowerState::ScreenOff
    );
    assert_eq!(
        client.power_state().expect("read resulting power state"),
        WebOsPowerState::ScreenOff
    );
    assert_eq!(server.snapshot().power_state, WebOsPowerState::ScreenOff);

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176461983
#[test]
fn screen_on_transitions_screen_off_tv_to_active() {
    let server = ObservedWebOsTvServer::screen_off(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert_eq!(
        client.power_state().expect("read initial power state"),
        WebOsPowerState::ScreenOff
    );
    assert_eq!(
        client.turn_screen_on().expect("turn screen on"),
        WebOsPowerState::Active
    );
    assert_eq!(
        client.power_state().expect("read resulting power state"),
        WebOsPowerState::Active
    );
    assert_eq!(server.snapshot().power_state, WebOsPowerState::Active);

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176461983
#[test]
fn screen_on_while_active_preserves_the_observed_substate_error() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert!(matches!(
        client.turn_screen_on(),
        Err(WebOsScreenControlError::Control {
            source: WebOsControlError::Request {
                source: WebOsClientError::WebOs {
                    code: Some(500),
                    message,
                    payload: Some(payload),
                },
            },
        }) if message == "Application error"
            && payload["errorCode"] == "-102"
            && payload["state"] == "Active"
    ));
    assert_eq!(
        client.power_state().expect("read unchanged power state"),
        WebOsPowerState::Active
    );

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176461983
#[test]
fn legacy_screen_endpoints_are_unavailable_and_do_not_change_state() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    for uri in [TURN_OFF_SCREEN_LEGACY_URI, TURN_ON_SCREEN_LEGACY_URI] {
        assert!(matches!(
            client.send_request(uri, json!({"standbyMode": "active"})),
            Err(WebOsClientError::WebOs {
                code: Some(404),
                message,
                payload: Some(payload),
            }) if message == "no such service or method" && payload == json!({})
        ));
        assert_eq!(
            client.power_state().expect("read unchanged power state"),
            WebOsPowerState::Active
        );
    }

    drop(client);
    server.finish();
}

// Real-TV wire observation:
// https://github.com/Staphylococcus/LG_Buddy/issues/51#issuecomment-5176463406
#[test]
fn power_off_transitions_active_tv_and_rejects_immediate_registration() {
    let server = ObservedWebOsTvServer::active(ObservedWebOsInput::Hdmi3);
    let mut client = server
        .connect_authenticated()
        .expect("connect to observed webOS TV");

    assert_eq!(
        client.power_state().expect("read initial power state"),
        WebOsPowerState::Active
    );
    client.power_off().expect("power off");
    assert_eq!(server.snapshot().power_state, WebOsPowerState::PowerOff);

    assert!(matches!(
        server.connect_authenticated(),
        Err(WebOsAuthenticatedClientError::Authentication {
            source: PlatformAccessTokenAcquisitionError::Registration {
                source: WebOsClientRegistrationError::Transport {
                    source: WebOsClientError::ConnectionClosed { .. },
                },
            },
        })
    ));
    assert_eq!(server.snapshot().connection_count, 2);
    server.finish();
}
