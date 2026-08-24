mod support;

use lg_buddy::config::HdmiInput;
use lg_buddy::tv::{
    BscpylgtvCommandClient, CurrentInput, OledBrightness, TvClient, TvErrorKind, TvPowerState,
};
use std::net::Ipv4Addr;
use support::MockBscpylgtv;

#[test]
fn mock_get_input_matches_real_shape() {
    let mock = MockBscpylgtv::new("mock-get-input");
    let client = mock_client(&mock);

    let input = client
        .current_input()
        .expect("mock get_input should succeed");

    assert_eq!(input, CurrentInput::Hdmi(HdmiInput::Hdmi3));
}

#[test]
fn mock_set_input_succeeds_and_updates_state() {
    let mock = MockBscpylgtv::new("mock-set-input");
    let client = mock_client(&mock);

    client
        .set_input(HdmiInput::Hdmi2)
        .expect("mock set_input should succeed");

    assert_eq!(mock.state_snapshot().input, "HDMI_2");
    assert!(mock.state_snapshot().screen_on);
}

#[test]
fn planned_set_input_success_preserves_normal_state_updates() {
    let mock = MockBscpylgtv::new("mock-planned-set-input");
    mock.set_power_on(false);
    mock.set_screen_on(false);
    mock.set_input("HDMI_1");
    mock.queue_set_input_wake_success();
    let client = mock_client(&mock);

    client
        .set_input(HdmiInput::Hdmi4)
        .expect("planned set_input should succeed");

    let state = mock.state_snapshot();
    assert!(state.power_on);
    assert!(state.screen_on);
    assert_eq!(state.input, "HDMI_4");
}

#[test]
fn mock_set_settings_updates_backlight() {
    let mock = MockBscpylgtv::new("mock-set-settings");
    let client = mock_client(&mock);

    client
        .set_oled_brightness(brightness(70))
        .expect("mock set_oled_brightness should succeed");

    assert_eq!(mock.state_snapshot().backlight, 70);
}

#[test]
fn mock_get_picture_settings_includes_backlight() {
    let mock = MockBscpylgtv::new("mock-get-picture-settings");
    mock.set_backlight(62);
    let client = mock_client(&mock);

    let brightness = client
        .oled_brightness()
        .expect("mock get_oled_brightness should succeed");

    assert_eq!(brightness.as_percent(), 62);
}

#[test]
fn mock_turn_screen_on_substate_error_matches_real_traceback_shape() {
    let mock = MockBscpylgtv::new("mock-turn-screen-on-substate");
    let client = mock_client(&mock);

    let err = client
        .unblank_screen()
        .expect_err("substate mismatch should fail");

    assert_eq!(err.kind(), TvErrorKind::ScreenUnblankSubstateMismatch);
    assert!(
        err.detail().contains("bscpylgtv.exceptions.PyLGTVCmdError"),
        "detail was: {}",
        err.detail()
    );
    assert!(
        err.detail().contains("errorCode': '-102'"),
        "detail was: {}",
        err.detail()
    );
}

#[test]
fn mock_tracks_screen_and_power_state_transitions() {
    let mock = MockBscpylgtv::new("mock-state-transitions");
    let client = mock_client(&mock);

    assert_eq!(
        client.power_state().expect("read active power state"),
        TvPowerState::Active
    );

    client
        .blank_screen()
        .expect("turn_screen_off should succeed");
    assert!(!mock.state_snapshot().screen_on);
    assert_eq!(
        client.power_state().expect("read blanked power state"),
        TvPowerState::ScreenOff
    );

    client
        .unblank_screen()
        .expect("turn_screen_on should succeed from blank state");
    assert!(mock.state_snapshot().screen_on);
    assert_eq!(
        client.power_state().expect("read restored power state"),
        TvPowerState::Active
    );

    client.power_off().expect("power_off should succeed");
    let state = mock.state_snapshot();
    assert!(!state.power_on);
    assert!(!state.screen_on);
}

#[test]
fn mock_rejects_input_queries_when_powered_off() {
    let mock = MockBscpylgtv::new("mock-powered-off-query");
    let client = mock_client(&mock);

    client.power_off().expect("power_off should succeed");

    let err = client
        .current_input()
        .expect_err("get_input should fail when off");

    assert_eq!(err.kind(), TvErrorKind::Rejected);
    assert!(err.detail().contains("TV is off"));
}

#[test]
fn mock_records_invocations_and_can_override_outputs() {
    let mock = MockBscpylgtv::new("mock-call-log");
    mock.queue_success("get_input", "\nignored\ncom.webos.app.hdmi2\n");
    let client = mock_client(&mock);

    let input = client
        .current_input()
        .expect("planned get_input should succeed");

    assert_eq!(input, CurrentInput::Hdmi(HdmiInput::Hdmi2));
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| (call.tv_ip, call.command, call.args))
            .collect::<Vec<_>>(),
        vec![(
            "10.0.0.39".to_string(),
            "get_input".to_string(),
            Vec::<String>::new(),
        )]
    );
}

fn mock_client(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
    BscpylgtvCommandClient::with_args(ip("10.0.0.39"), mock.command_path(), mock.command_args())
}

fn ip(value: &str) -> Ipv4Addr {
    value.parse().expect("parse IPv4 address")
}

fn brightness(value: u8) -> OledBrightness {
    OledBrightness::new(value).expect("test brightness should be valid")
}
