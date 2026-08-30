use crate::cucumber_support::world::LgBuddyWorld;
use cucumber::{given, then, when};

#[given(regex = r#"a temporary LG Buddy config using input (HDMI_[1-4])"#)]
fn temporary_config(world: &mut LgBuddyWorld, input: String) {
    world.create_config(&input);
}

#[given("an empty temporary LG Buddy config path")]
fn empty_temporary_config_path(world: &mut LgBuddyWorld) {
    world.create_empty_config_path();
}

#[given(regex = r#"the screen restore policy is "(marker_only|conservative|aggressive)""#)]
fn screen_restore_policy(world: &mut LgBuddyWorld, policy: String) {
    world.set_screen_restore_policy(&policy);
}

#[given(regex = r#"screen idle blanking is "(enabled|disabled)""#)]
fn screen_idle_blanking(world: &mut LgBuddyWorld, policy: String) {
    world.set_screen_idle_blank(&policy);
}

#[given(regex = r#"the idle timeout is (\d+) seconds"#)]
fn idle_timeout_seconds(world: &mut LgBuddyWorld, seconds: u64) {
    world.set_idle_timeout_secs(seconds);
}

#[given("the current config is remembered")]
fn current_config_is_remembered(world: &mut LgBuddyWorld) {
    world.remember_config_contents();
}

#[given("systemd apply actions are skipped")]
fn systemd_apply_actions_are_skipped(world: &mut LgBuddyWorld) {
    world.skip_systemd_apply_actions();
}

#[given("the user screen service is active")]
fn user_screen_service_is_active(world: &mut LgBuddyWorld) {
    world.install_active_user_screen_service_stub();
}

#[given("LG Buddy session runtime is isolated")]
fn isolated_runtime(world: &mut LgBuddyWorld) {
    world.create_runtime();
}

#[given("a mock TV client")]
fn mock_tv_client(world: &mut LgBuddyWorld) {
    world.create_mock_tv();
}

#[given(regex = r#"a native webOS TV on input (HDMI_[23]) with brightness (\d+)"#)]
fn native_webos_tv(world: &mut LgBuddyWorld, input: String, brightness: u8) {
    world.create_native_webos_tv(&input, brightness);
}

#[given(
    regex = r#"a native webOS26 TV on firmware 43\.21\.60 on input (HDMI_[23]) with brightness (\d+)"#
)]
fn webos26_firmware_43_21_60_tv(world: &mut LgBuddyWorld, input: String, brightness: u8) {
    world.create_webos26_firmware_43_21_60_tv(&input, brightness);
}

#[given(regex = r#"the existing config selects TV platform \"(bscpylgtv|lg_webos)\""#)]
fn existing_tv_platform(world: &mut LgBuddyWorld, platform: String) {
    world.select_tv_platform(&platform);
}

#[given("a valid native TV access token is stored")]
fn valid_native_access_token(world: &mut LgBuddyWorld) {
    world.store_valid_native_access_token();
}

#[given("a stale native TV access token is stored")]
fn stale_native_access_token(world: &mut LgBuddyWorld) {
    world.store_native_access_token("stale-cucumber-access-token");
}

#[given("the native webOS TV rejects pairing")]
fn native_webos_tv_rejects_pairing(world: &mut LgBuddyWorld) {
    world.reject_native_pairing();
}

#[given("the native webOS TV stalls its first TV response")]
fn native_webos_tv_stalls_first_response(world: &mut LgBuddyWorld) {
    world.stall_native_tv_response();
}

#[given("the native webOS TV interrupts the first restore session and acknowledges input without unblanking")]
fn native_webos_tv_has_ambiguous_restore(world: &mut LgBuddyWorld) {
    world.make_native_restore_ambiguous();
}

#[given(regex = r#"mock system logind reports PreparingForSleep=(true|false)"#)]
fn mock_system_logind_preparing_for_sleep(world: &mut LgBuddyWorld, value: String) {
    world.configure_system_logind(value == "true");
}

#[given(regex = r#"the TV auth key file override is "([^"]+)""#)]
fn tv_auth_key_file_override(world: &mut LgBuddyWorld, path: String) {
    world.set_auth_key_file_override(&path);
}

#[given("the inherited user environment is cleared")]
fn inherited_user_environment_is_cleared(world: &mut LgBuddyWorld) {
    world.clear_inherited_user_env();
}

#[given("the TV is reachable over ping")]
fn tv_is_reachable_over_ping(world: &mut LgBuddyWorld) {
    world.install_ping_stub(true);
}

#[given("the TV is unreachable over ping")]
fn tv_is_unreachable_over_ping(world: &mut LgBuddyWorld) {
    world.install_ping_stub(false);
}

#[given(regex = r#"the TV is on input (HDMI_[1-4])"#)]
fn tv_on_input(world: &mut LgBuddyWorld, input: String) {
    world.tv_mut().set_input(&input);
}

#[given(regex = r#"the TV backlight is (\d+)"#)]
fn tv_backlight(world: &mut LgBuddyWorld, value: u8) {
    world.tv_mut().set_backlight(u64::from(value));
}

#[given(regex = r#"the TV volume is (\d+)"#)]
fn tv_volume(world: &mut LgBuddyWorld, value: u8) {
    world.set_tv_volume(value);
}

#[given("the TV volume is unknown")]
fn tv_volume_is_unknown(world: &mut LgBuddyWorld) {
    world.set_tv_volume_unknown();
}

#[given(regex = r#"the TV is (muted|unmuted)"#)]
fn tv_mute_state(world: &mut LgBuddyWorld, state: String) {
    world.set_tv_muted(state == "muted");
}

#[given(regex = r#"the brightness dialog returns (\d+)"#)]
fn brightness_dialog_returns(world: &mut LgBuddyWorld, value: u8) {
    world.install_brightness_ui_stub(Some(value));
}

#[given("the brightness dialog is cancelled")]
fn brightness_dialog_is_cancelled(world: &mut LgBuddyWorld) {
    world.install_brightness_ui_stub(None);
}

#[given("the brightness error dialog is available")]
fn brightness_error_dialog_is_available(world: &mut LgBuddyWorld) {
    world.install_brightness_ui_stub(None);
}

#[given("the TV screen is blanked")]
fn tv_screen_blanked(world: &mut LgBuddyWorld) {
    world.tv_mut().set_screen_on(false);
}

#[given("the TV is powered off")]
fn tv_powered_off_given(world: &mut LgBuddyWorld) {
    world.tv_mut().set_power_on(false);
    world.tv_mut().set_screen_on(false);
}

#[given("the session marker exists")]
fn session_marker_exists_given(world: &mut LgBuddyWorld) {
    world.create_session_marker();
}

#[given("the system marker exists")]
fn system_marker_exists_given(world: &mut LgBuddyWorld) {
    world.create_system_marker();
}

#[given(regex = r#"the TV will fail "([^"]+)" with status (\d+) and stderr "([^"]+)""#)]
fn tv_failure(world: &mut LgBuddyWorld, command: String, status: u64, stderr: String) {
    world.tv_mut().queue_error(&command, status as i64, &stderr);
}

#[given(regex = r#"the TV will fail "([^"]+)" (\d+) times with status (\d+) and stderr "([^"]+)""#)]
fn tv_failure_repeated(
    world: &mut LgBuddyWorld,
    command: String,
    times: u64,
    status: u64,
    stderr: String,
) {
    for _ in 0..times {
        world.tv_mut().queue_error(&command, status as i64, &stderr);
    }
}

#[given("the executable PATH is isolated")]
fn executable_path_isolated(world: &mut LgBuddyWorld) {
    world.isolate_path();
}

#[given("GNOME Shell is available")]
fn gnome_shell_available(world: &mut LgBuddyWorld) {
    world.install_gnome_shell_stub();
}

#[given("GNOME idle monitor is unavailable")]
fn gnome_idle_monitor_unavailable(world: &mut LgBuddyWorld) {
    world.set_gnome_idle_monitor_available(false);
}

#[given("GNOME reports the session idle")]
fn gnome_reports_idle(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_idle();
}

#[given("GNOME reports the session active")]
fn gnome_reports_active(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_active();
}

#[given("GNOME requests screen wake")]
fn gnome_requests_screen_wake(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_wake_requested();
}

#[given("GNOME emits no ScreenSaver signals")]
fn gnome_emits_no_screen_saver_signals(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emits_no_screen_saver_signals();
}

#[given(regex = r#"GNOME idle monitor will report idletimes "([^"]+)""#)]
fn gnome_idle_monitor_reports_idletimes(world: &mut LgBuddyWorld, values: String) {
    let parsed = values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("invalid idletime `{value}`: {err}"))
        })
        .collect::<Vec<_>>();

    assert!(
        !parsed.is_empty(),
        "expected at least one GNOME idle-monitor idletime value"
    );

    world.gnome_idle_monitor_reports_idletimes(&parsed);
}

#[given(regex = r#"GNOME monitor stays open for ([0-9]+(?:\.[0-9]+)?) seconds"#)]
fn gnome_monitor_stays_open_for_seconds(world: &mut LgBuddyWorld, seconds: String) {
    let seconds = seconds
        .parse::<f64>()
        .unwrap_or_else(|err| panic!("invalid GNOME monitor sleep `{seconds}`: {err}"));
    world.gnome_monitor_stays_open_for_secs(seconds);
}

#[given(regex = r#"gamepad activity is observed after ([0-9]+(?:\.[0-9]+)?) seconds"#)]
fn gamepad_activity_is_observed_after_seconds(world: &mut LgBuddyWorld, seconds: String) {
    let seconds = seconds
        .parse::<f64>()
        .unwrap_or_else(|err| panic!("invalid gamepad activity delay `{seconds}`: {err}"));
    world.gamepad_activity_occurs_after_secs(seconds);
}

#[given("swayidle is installed")]
fn swayidle_installed(world: &mut LgBuddyWorld) {
    world.install_swayidle_stub();
}

#[given("swayidle will emit an idle timeout")]
fn swayidle_will_emit_timeout(world: &mut LgBuddyWorld) {
    world.swayidle_emits_timeout();
}

#[given("swayidle will emit a resume event")]
fn swayidle_will_emit_resume(world: &mut LgBuddyWorld) {
    world.swayidle_emits_resume();
}

#[given("the next input restore attempt powers the TV back on")]
fn next_input_restore_attempt_powers_tv_on(world: &mut LgBuddyWorld) {
    world.tv_mut().queue_set_input_wake_success();
}

#[given("the next input restore attempt is acknowledged without unblanking")]
fn next_input_restore_attempt_is_acknowledged_without_unblanking(world: &mut LgBuddyWorld) {
    world.tv_mut().queue_set_input_ack_without_screen_on();
}

#[given(regex = r#"the backend override is "([^"]+)""#)]
fn backend_override(world: &mut LgBuddyWorld, backend: String) {
    world.set_backend_override(&backend);
}

#[given("startup delays are disabled")]
fn startup_delays_disabled(world: &mut LgBuddyWorld) {
    world.disable_startup_delays();
}

#[given("screen wake delays are disabled")]
fn screen_wake_delays_disabled(world: &mut LgBuddyWorld) {
    world.disable_screen_wake_delays();
}

#[given("nm-online succeeds")]
fn nm_online_succeeds(world: &mut LgBuddyWorld) {
    world.install_nm_online_stub(0);
}

#[given(regex = r#"nm-online fails with status (\d+)"#)]
fn nm_online_fails(world: &mut LgBuddyWorld, status: u64) {
    world.install_nm_online_stub(status as i64);
}

#[given("sleep retry delays are disabled")]
fn sleep_retry_delays_disabled(world: &mut LgBuddyWorld) {
    world.disable_sleep_delays();
}

#[given("reboot detection reports no pending reboot")]
fn reboot_not_pending(world: &mut LgBuddyWorld) {
    world.install_systemctl_stub(false);
}

#[given("reboot detection reports a pending reboot")]
fn reboot_pending(world: &mut LgBuddyWorld) {
    world.install_systemctl_stub(true);
}

#[given("journalctl reports a pending NetworkManager sleep request")]
fn journalctl_reports_sleep_requested(world: &mut LgBuddyWorld) {
    world.install_journalctl_stub(true);
}

#[given("journalctl does not report a pending NetworkManager sleep request")]
fn journalctl_reports_no_sleep_requested(world: &mut LgBuddyWorld) {
    world.install_journalctl_stub(false);
}

#[when(regex = r#"I run the command "([^"]+)""#)]
fn run_command(world: &mut LgBuddyWorld, command: String) {
    world.run_named_command(&command);
}

#[when("I choose native webOS during initial configuration")]
fn run_native_initial_configuration(world: &mut LgBuddyWorld) {
    world.run_native_initial_configuration();
}

#[then("the command succeeds")]
fn command_succeeds(world: &mut LgBuddyWorld) {
    assert!(
        world.command_result().success,
        "command failed\nstdout:\n{}\nstderr:\n{}",
        world.command_result().stdout,
        world.command_result().stderr
    );
}

#[then("the command fails")]
fn command_fails(world: &mut LgBuddyWorld) {
    assert!(
        !world.command_result().success,
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        world.command_result().stdout,
        world.command_result().stderr
    );
}

#[then(regex = r#"^the command exits with status (\d+)$"#)]
fn command_exits_with_status(world: &mut LgBuddyWorld, expected: i32) {
    assert_eq!(
        world.command_result().exit_code,
        Some(expected),
        "unexpected command exit status\nstdout:\n{}\nstderr:\n{}",
        world.command_result().stdout,
        world.command_result().stderr
    );
}

#[then(regex = r#"the command completes within (\d+) seconds"#)]
fn command_completes_within_seconds(world: &mut LgBuddyWorld, seconds: u64) {
    assert!(
        world.command_duration() < std::time::Duration::from_secs(seconds),
        "command took {:?}, expected less than {seconds}s",
        world.command_duration()
    );
}

#[then(regex = r#"stdout contains "([^"]+)""#)]
fn stdout_contains(world: &mut LgBuddyWorld, expected: String) {
    assert!(
        world.command_result().stdout.contains(&expected),
        "stdout was: {}",
        world.command_result().stdout
    );
}

#[then(regex = r#"stdout does not contain "([^"]+)""#)]
fn stdout_does_not_contain(world: &mut LgBuddyWorld, unexpected: String) {
    assert!(
        !world.command_result().stdout.contains(&unexpected),
        "stdout was: {}",
        world.command_result().stdout
    );
}

#[then(regex = r#"stderr contains "([^"]+)""#)]
fn stderr_contains(world: &mut LgBuddyWorld, expected: String) {
    assert!(
        world.command_result().stderr.contains(&expected),
        "stderr was: {}",
        world.command_result().stderr
    );
}

#[then(regex = r#"stderr does not contain "([^"]+)""#)]
fn stderr_does_not_contain(world: &mut LgBuddyWorld, unexpected: String) {
    assert!(
        !world.command_result().stderr.contains(&unexpected),
        "stderr was: {}",
        world.command_result().stderr
    );
}

#[then(regex = r#"config\.env contains "([^"]+)""#)]
fn config_env_contains(world: &mut LgBuddyWorld, expected: String) {
    world.assert_config_contains(&expected);
}

#[then(regex = r#"config\.env does not contain "([^"]+)""#)]
fn config_env_does_not_contain(world: &mut LgBuddyWorld, unexpected: String) {
    world.assert_config_does_not_contain(&unexpected);
}

#[then("config.env is unchanged")]
fn config_env_is_unchanged(world: &mut LgBuddyWorld) {
    world.assert_config_unchanged();
}

#[then(regex = r#"systemctl was invoked with "([^"]+)""#)]
fn systemctl_was_invoked_with(world: &mut LgBuddyWorld, expected: String) {
    world.assert_systemctl_invoked_with(&expected);
}

#[then(regex = r#"nm-online was invoked with "([^"]+)""#)]
fn nm_online_invoked_with(world: &mut LgBuddyWorld, expected: String) {
    let argv = expected.split_whitespace().collect::<Vec<_>>();
    world.assert_nm_online_invoked_with(&argv);
}

#[then(regex = r#"stdout is "([^"]+)""#)]
fn stdout_is(world: &mut LgBuddyWorld, expected: String) {
    assert_eq!(world.command_result().stdout.trim(), expected);
}

#[then("the session marker exists")]
fn session_marker_exists_then(world: &mut LgBuddyWorld) {
    world.runtime().assert_session_marker_exists();
}

#[then("the session marker is absent")]
fn session_marker_absent(world: &mut LgBuddyWorld) {
    world.runtime().assert_session_marker_absent();
}

#[then("the system marker exists")]
fn system_marker_exists_then(world: &mut LgBuddyWorld) {
    world.runtime().assert_system_marker_exists();
}

#[then("the system marker is absent")]
fn system_marker_absent(world: &mut LgBuddyWorld) {
    world.runtime().assert_system_marker_absent();
}

#[then(regex = r#"the TV input is (HDMI_[1-4])"#)]
fn tv_input_is(world: &mut LgBuddyWorld, input: String) {
    world.assert_tv_input(&input);
}

#[then(regex = r#"the TV brightness is (\d+)"#)]
fn tv_brightness_is(world: &mut LgBuddyWorld, value: u8) {
    world.assert_tv_brightness(value);
}

#[then(regex = r#"the TV volume is (\d+)"#)]
fn tv_volume_is(world: &mut LgBuddyWorld, value: u8) {
    world.assert_tv_volume(value);
}

#[then(regex = r#"the TV is (muted|unmuted)"#)]
fn tv_mute_state_is(world: &mut LgBuddyWorld, state: String) {
    world.assert_tv_muted(state == "muted");
}

#[then("the TV is powered off")]
fn tv_is_powered_off(world: &mut LgBuddyWorld) {
    world.assert_tv_powered_on(false);
}

#[then("the TV is powered on")]
fn tv_is_powered_on(world: &mut LgBuddyWorld) {
    world.assert_tv_powered_on(true);
}

#[then("the TV screen is blanked")]
fn tv_screen_is_blanked(world: &mut LgBuddyWorld) {
    world.assert_tv_screen_on(false);
}

#[then("the TV screen is visible")]
fn tv_screen_is_visible(world: &mut LgBuddyWorld) {
    world.assert_tv_screen_on(true);
}

#[then(regex = r#"^the TV client received "([^"]+)"$"#)]
fn tv_client_received(world: &mut LgBuddyWorld, command: String) {
    let calls = world.tv_call_names();
    assert!(
        calls.iter().any(|call| call == &command),
        "calls were: {calls:?}"
    );
}

#[then(regex = r#"^the TV client received "([^"]+)" exactly (\d+) times$"#)]
fn tv_client_received_exactly(world: &mut LgBuddyWorld, command: String, expected: usize) {
    let calls = world.tv_call_names();
    let actual = calls.iter().filter(|call| *call == &command).count();

    assert_eq!(actual, expected, "calls were: {calls:?}");
}

#[then(regex = r#"^the TV client did not receive "([^"]+)"$"#)]
fn tv_client_did_not_receive(world: &mut LgBuddyWorld, command: String) {
    let calls = world.tv_call_names();
    assert!(
        calls.iter().all(|call| call != &command),
        "calls were: {calls:?}"
    );
}

#[then("a valid native TV access token is stored")]
fn valid_native_access_token_is_stored(world: &mut LgBuddyWorld) {
    world.assert_valid_native_access_token();
}

#[then("no native TV access token is stored")]
fn no_native_access_token_is_stored(world: &mut LgBuddyWorld) {
    world.assert_no_native_access_token();
}

#[then(regex = r#"the native TV access token is \"([^\"]+)\""#)]
fn native_access_token_is(world: &mut LgBuddyWorld, access_token: String) {
    world.assert_native_access_token(&access_token);
}

#[then(regex = r#"the native TV connection count is (\d+)"#)]
fn native_connection_count_is(world: &mut LgBuddyWorld, expected: u64) {
    assert_eq!(world.webos_snapshot().connection_count, expected);
}

#[then(regex = r#"the native TV pairing prompt count is (\d+)"#)]
fn native_pairing_prompt_count_is(world: &mut LgBuddyWorld, expected: u64) {
    assert_eq!(world.webos_snapshot().pairing_prompt_count, expected);
}

#[then(regex = r#"the native TV registration tokens are \"([^\"]*)\""#)]
fn native_registration_tokens_are(world: &mut LgBuddyWorld, expected: String) {
    let expected = if expected.is_empty() {
        Vec::new()
    } else {
        expected
            .split(',')
            .map(|token| match token.trim() {
                "none" => None,
                token => Some(token.to_string()),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(world.webos_snapshot().registration_tokens, expected);
}

#[then("the TV helper uses the expected auth context")]
fn tv_helper_uses_expected_auth_context(world: &mut LgBuddyWorld) {
    world.assert_tv_calls_match_expected_auth_context();
}
