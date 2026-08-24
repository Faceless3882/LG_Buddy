mod support;

use lg_buddy::commands::{run_screen_off, run_screen_on, run_system_resume};
use lg_buddy::session::runner::{RuntimeActionExecutor, SessionEventDispatcher};
use lg_buddy::session::SessionEvent;
use lg_buddy::settings::SettingsCommand;
use lg_buddy::{run_command, Command};
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use support::{
    ExecutableScript, MockBscpylgtv, MockNmOnline, MockSystemLogind, RuntimeStateLayout,
    TestConfigFile, TestEnv,
};

#[test]
fn run_screen_off_loads_config_and_uses_session_runtime_override() {
    let mock = MockBscpylgtv::new("entrypoint-screen-off-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-screen-off-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-off-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-off-runtime");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_off(&mut output).expect("screen-off should succeed");

    runtime.assert_session_marker_exists();
    let calls = mock.calls();
    assert_eq!(
        calls
            .iter()
            .cloned()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["get_input".to_string(), "turn_screen_off".to_string()]
    );
    let expected_key_path = config
        .path()
        .parent()
        .expect("config parent")
        .join(".aiopylgtv.sqlite");
    assert_eq!(
        calls.first().and_then(|call| call.key_file_path.as_deref()),
        Some(expected_key_path.to_str().expect("utf8 key path"))
    );
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Screen blank command succeeded."));
}

#[test]
fn run_screen_on_loads_config_and_clears_session_marker() {
    let mock = MockBscpylgtv::new("entrypoint-screen-on-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-screen-on-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-on-config");
    config.write_sample("HDMI_3");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-on-runtime");
    runtime.create_session_marker();

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should succeed");

    runtime.assert_session_marker_absent();
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec![
            "get_power_state".to_string(),
            "turn_screen_on".to_string(),
            "get_power_state".to_string(),
        ]
    );
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Screen unblank succeeded."));
}

#[test]
fn run_screen_on_loads_aggressive_config_and_restores_without_session_marker() {
    let mock = MockBscpylgtv::new("entrypoint-screen-on-aggressive-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-screen-on-aggressive-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-on-aggressive-config");
    config.write_sample("HDMI_3");
    config.append_line("screen_restore_policy=aggressive");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-on-aggressive-runtime");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should restore in aggressive mode");

    runtime.assert_session_marker_absent();
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec![
            "get_power_state".to_string(),
            "turn_screen_on".to_string(),
            "get_power_state".to_string(),
        ]
    );
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Aggressive restore policy is enabled"));
    assert!(output.contains("Screen unblank succeeded."));
}

#[test]
fn settings_set_restore_policy_is_loaded_by_screen_runtime() {
    let mock = MockBscpylgtv::new("entrypoint-settings-set-runtime-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-settings-set-runtime-wrapper");

    let config = TestConfigFile::new("entrypoint-settings-set-runtime-config");
    config.write_sample("HDMI_3");
    let runtime = RuntimeStateLayout::new("entrypoint-settings-set-runtime-state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
    env.set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

    let mut settings_output = Vec::new();
    run_command(
        Command::Settings(SettingsCommand::Set {
            key: "screen.restore_policy".to_string(),
            value: "aggressive".to_string(),
        }),
        &mut settings_output,
    )
    .expect("settings set should succeed");

    assert!(fs::read_to_string(config.path())
        .expect("read config")
        .contains("screen_restore_policy=aggressive\n"));
    assert!(String::from_utf8(settings_output)
        .expect("settings output utf8")
        .contains("apply: skipped systemd apply"));

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should use settings-written policy");

    runtime.assert_session_marker_absent();
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec![
            "get_power_state".to_string(),
            "turn_screen_on".to_string(),
            "get_power_state".to_string(),
        ]
    );
    assert!(String::from_utf8(output)
        .expect("screen output utf8")
        .contains("Aggressive restore policy is enabled"));
}

#[test]
fn settings_unset_restore_policy_is_loaded_as_screen_runtime_default() {
    let mock = MockBscpylgtv::new("entrypoint-settings-unset-runtime-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-settings-unset-runtime-wrapper");

    let config = TestConfigFile::new("entrypoint-settings-unset-runtime-config");
    config.write_sample("HDMI_3");
    config.append_line("screen_restore_policy=aggressive");
    let runtime = RuntimeStateLayout::new("entrypoint-settings-unset-runtime-state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
    env.set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

    let mut settings_output = Vec::new();
    run_command(
        Command::Settings(SettingsCommand::Unset("screen.restore_policy".to_string())),
        &mut settings_output,
    )
    .expect("settings unset should succeed");

    assert!(!fs::read_to_string(config.path())
        .expect("read config")
        .contains("screen_restore_policy="));
    assert!(String::from_utf8(settings_output)
        .expect("settings output utf8")
        .contains("screen.restore_policy unset"));

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should use default conservative policy");

    runtime.assert_session_marker_absent();
    assert!(mock.calls().is_empty());
    assert!(String::from_utf8(output)
        .expect("screen output utf8")
        .contains("State file not found"));
}

#[test]
fn settings_set_screen_key_restarts_active_user_screen_service() {
    let config = TestConfigFile::new("entrypoint-settings-apply-config");
    config.write_sample("HDMI_3");
    let systemctl_log = config.path().with_file_name("systemctl.log");
    let systemctl = ExecutableScript::new(
        "entrypoint-settings-apply-systemctl",
        "systemctl",
        &format!(
            "#!/bin/sh\n\
printf '%s\\n' \"$*\" >> '{}'\n\
case \"$2\" in\n\
  cat|is-active|is-enabled|restart) exit 0 ;;\n\
  *) exit 1 ;;\n\
esac\n",
            systemctl_log.display()
        ),
    );

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    env.remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");

    let mut output = Vec::new();
    run_command(
        Command::Settings(SettingsCommand::Set {
            key: "screen.idle_timeout".to_string(),
            value: "600".to_string(),
        }),
        &mut output,
    )
    .expect("settings set should restart active user service");

    assert!(fs::read_to_string(config.path())
        .expect("read config")
        .contains("screen_idle_timeout=600\n"));
    assert!(String::from_utf8(output)
        .expect("settings output utf8")
        .contains("apply: restarted LG_Buddy_screen.service"));

    let systemctl_calls = fs::read_to_string(systemctl_log).expect("read systemctl log");
    assert!(systemctl_calls.contains("--user cat LG_Buddy_screen.service"));
    assert!(systemctl_calls.contains("--user is-active --quiet LG_Buddy_screen.service"));
    assert!(systemctl_calls.contains("--user restart LG_Buddy_screen.service"));
}

#[test]
fn settings_set_lifecycle_policy_updates_config_without_systemd_apply() {
    let config = TestConfigFile::new("entrypoint-settings-lifecycle-policy-config");
    config.write_sample("HDMI_3");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());

    let mut output = Vec::new();
    run_command(
        Command::Settings(SettingsCommand::Set {
            key: "system.sleep_wake_policy".to_string(),
            value: "disabled".to_string(),
        }),
        &mut output,
    )
    .expect("lifecycle policy should be writable");

    assert!(fs::read_to_string(config.path())
        .expect("read config")
        .contains("system_sleep_wake_policy=disabled\n"));
    let output = String::from_utf8(output).expect("settings output utf8");
    assert!(output.contains("system.sleep_wake_policy=disabled"));
    assert!(output.contains("apply: no runtime apply action required"));
}

#[test]
fn run_system_resume_clears_sleep_cycle_state_and_preserves_session_marker() {
    let mock = MockBscpylgtv::new("entrypoint-system-resume-tv");
    let wrapper = mock.command_wrapper("entrypoint-system-resume-wrapper");
    let nm_online = MockNmOnline::new("entrypoint-system-resume-nm-online");
    let nm_online_wrapper = nm_online.command_wrapper("entrypoint-system-resume-nm-online-wrapper");

    let config = TestConfigFile::new("entrypoint-system-resume-config");
    config.write_sample("HDMI_4");

    let runtime = RuntimeStateLayout::new("entrypoint-system-resume-runtime");
    runtime.create_session_marker();
    runtime.create_system_marker();
    let attempt_marker = runtime.system_dir().join("system_sleep_attempted");
    let cycle_state = runtime.system_dir().join("system_sleep_cycle");
    fs::write(&attempt_marker, "").expect("create attempt marker");
    fs::write(&cycle_state, "outcome=completed\n").expect("create cycle state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_NM_ONLINE", nm_online_wrapper.path());
    env.set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");

    let mut output = Vec::new();
    run_system_resume(&mut output).expect("system resume should succeed");

    runtime.assert_system_marker_absent();
    runtime.assert_session_marker_exists();
    assert!(!attempt_marker.exists());
    assert!(!cycle_state.exists());
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["set_input".to_string()]
    );
    assert_eq!(nm_online.invocations().len(), 1);
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
}

#[test]
fn run_system_resume_without_native_token_retires_all_system_sleep_state() {
    let config = TestConfigFile::new("entrypoint-native-resume-missing-token-config");
    config.write_sample("HDMI_4");
    config.append_line("tvs_primary_platform=lg_webos");

    let runtime = RuntimeStateLayout::new("entrypoint-native-resume-missing-token-runtime");
    runtime.create_system_marker();
    runtime.create_system_sleep_attempt_marker();
    let cycle_state = runtime.system_dir().join("system_sleep_cycle");
    fs::write(&cycle_state, "outcome=completed\n").expect("create completed cycle state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    run_system_resume(&mut output).expect("missing native token should skip resume");

    runtime.assert_system_marker_absent();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert!(!cycle_state.exists());
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("No stored native TV credential; skipping unattended TV control."));
}

#[test]
fn run_system_resume_cleans_sleep_state_when_native_token_is_malformed() {
    let config = TestConfigFile::new("entrypoint-native-resume-malformed-token-config");
    config.write_sample("HDMI_4");
    config.append_line("tvs_primary_platform=lg_webos");
    let token_path = config
        .path()
        .parent()
        .expect("config parent")
        .join("tvs/primary/access-token.json");
    fs::create_dir_all(token_path.parent().expect("token parent")).expect("create token directory");
    fs::write(&token_path, "{ malformed").expect("write malformed token");

    let runtime = RuntimeStateLayout::new("entrypoint-native-resume-malformed-token-runtime");
    runtime.create_system_marker();
    runtime.create_system_sleep_attempt_marker();
    let cycle_state = runtime.system_dir().join("system_sleep_cycle");
    fs::write(&cycle_state, "outcome=completed\n").expect("create completed cycle state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    let error = run_system_resume(&mut output)
        .expect_err("malformed native token should remain an actionable error");

    assert!(error.to_string().contains("malformed"));
    runtime.assert_system_marker_absent();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert!(!cycle_state.exists());
}

#[test]
fn run_system_resume_aggressive_policy_restores_without_system_marker() {
    let mock = MockBscpylgtv::new("entrypoint-system-resume-aggressive-tv");
    let wrapper = mock.command_wrapper("entrypoint-system-resume-aggressive-wrapper");
    let nm_online = MockNmOnline::new("entrypoint-system-resume-aggressive-nm-online");
    let nm_online_wrapper =
        nm_online.command_wrapper("entrypoint-system-resume-aggressive-nm-online-wrapper");

    let config = TestConfigFile::new("entrypoint-system-resume-aggressive-config");
    config.write_sample("HDMI_4");
    config.append_line("screen_restore_policy=aggressive");

    let runtime = RuntimeStateLayout::new("entrypoint-system-resume-aggressive-runtime");
    let cycle_state = runtime.system_dir().join("system_sleep_cycle");
    fs::create_dir_all(runtime.system_dir()).expect("create system runtime dir");
    fs::write(&cycle_state, "outcome=completed\n").expect("create cycle state");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_NM_ONLINE", nm_online_wrapper.path());
    env.set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");

    let mut output = Vec::new();
    run_system_resume(&mut output).expect("aggressive system resume should succeed");

    runtime.assert_system_marker_absent();
    assert!(!cycle_state.exists());
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["set_input".to_string()]
    );
    assert_eq!(nm_online.invocations().len(), 1);
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("State file not found. Aggressive restore policy is enabled"));
    assert!(output.contains("Wake from sleep: Restoring display state."));
}

#[test]
fn run_nm_pre_down_uses_logind_property_and_retries_idempotently() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-nm-pre-down-logind");
    logind.reset();
    logind.set_preparing_for_sleep(true);

    let mock = MockBscpylgtv::new("entrypoint-nm-pre-down-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-nm-pre-down-wrapper");

    let config = TestConfigFile::new("entrypoint-nm-pre-down-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-nm-pre-down-runtime");

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut first_output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut first_output)
        .expect("NetworkManager pre-down should succeed during system sleep");

    runtime.assert_system_marker_exists();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert_eq!(
        mock.calls()
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        vec!["get_input", "power_off"]
    );
    assert!(!mock.state_snapshot().power_on);
    assert!(String::from_utf8(first_output)
        .expect("utf8 output")
        .contains("logind is preparing for sleep"));

    let mut second_output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut second_output)
        .expect("repeated NetworkManager pre-down should stay idempotent");

    assert_eq!(
        mock.calls()
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        vec!["get_input", "power_off"]
    );
    runtime.assert_system_marker_exists();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert!(String::from_utf8(second_output)
        .expect("utf8 output")
        .contains("logind is preparing for sleep"));
}

#[test]
fn run_nm_pre_down_skips_network_disconnect_and_clears_stale_attempt() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-nm-pre-down-not-sleeping-logind");
    logind.reset();
    logind.set_preparing_for_sleep(false);

    let mock = MockBscpylgtv::new("entrypoint-nm-pre-down-not-sleeping-tv");
    let wrapper = mock.command_wrapper("entrypoint-nm-pre-down-not-sleeping-wrapper");

    let config = TestConfigFile::new("entrypoint-nm-pre-down-not-sleeping-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-nm-pre-down-not-sleeping-runtime");
    runtime.create_system_sleep_attempt_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut output)
        .expect("ordinary NetworkManager pre-down should fail open");

    runtime.assert_system_sleep_attempt_marker_absent();
    runtime.assert_system_marker_absent();
    assert!(mock.calls().is_empty());
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("not preparing for sleep"));
}

#[test]
fn session_dispatcher_skips_screen_action_while_logind_reports_sleep_pending() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-monitor-sleep-pending-logind");
    logind.reset();
    logind.set_preparing_for_sleep(true);

    let mock = MockBscpylgtv::new("entrypoint-monitor-sleep-pending-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-monitor-sleep-pending-wrapper");

    let config = TestConfigFile::new("entrypoint-monitor-sleep-pending-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-monitor-sleep-pending-runtime");

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor);
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Idle)
        .expect("session idle dispatch should succeed");

    runtime.assert_session_marker_absent();
    assert!(mock.calls().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("Machine sleep is pending"),
        "output was: {output}"
    );
    assert!(
        output.contains("Skipping session screen action"),
        "output was: {output}"
    );
}

#[test]
fn session_dispatcher_skips_screen_restore_while_system_resume_restore_is_pending() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-monitor-system-restore-pending-logind");
    logind.reset();
    logind.set_preparing_for_sleep(false);

    let mock = MockBscpylgtv::new("entrypoint-monitor-system-restore-pending-tv");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-monitor-system-restore-pending-wrapper");

    let config = TestConfigFile::new("entrypoint-monitor-system-restore-pending-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-monitor-system-restore-pending-runtime");
    runtime.create_session_marker();
    runtime.create_system_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor);
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Active)
        .expect("session active dispatch should succeed");

    runtime.assert_session_marker_exists();
    runtime.assert_system_marker_exists();
    assert!(mock.calls().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("System resume restore is pending"),
        "output was: {output}"
    );
    assert!(
        output.contains("Skipping session screen action"),
        "output was: {output}"
    );
}

#[test]
fn run_lifecycle_monitor_uses_logind_resume_signal_and_runtime_restore() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-lifecycle-logind");
    logind.reset();
    let mock = MockBscpylgtv::new("entrypoint-lifecycle-tv");
    let wrapper = mock.command_wrapper("entrypoint-lifecycle-wrapper");
    let nm_online = MockNmOnline::new("entrypoint-lifecycle-nm-online");
    let nm_online_wrapper = nm_online.command_wrapper("entrypoint-lifecycle-nm-online-wrapper");

    let config = TestConfigFile::new("entrypoint-lifecycle-config");
    config.write_sample("HDMI_4");

    let runtime = RuntimeStateLayout::new("entrypoint-lifecycle-runtime");
    runtime.create_system_marker();
    runtime.create_system_sleep_attempt_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_NM_ONLINE", nm_online_wrapper.path());
    env.set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");
    env.set("LG_BUDDY_LIFECYCLE_MONITOR_TEST_EVENT_LIMIT", "1");

    let (done_tx, done_rx) = mpsc::channel();
    let lifecycle_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let result = run_command(Command::Lifecycle, &mut output).map_err(|err| err.to_string());
        done_tx
            .send((result, output))
            .expect("send lifecycle result");
    });

    wait_until(Duration::from_secs(4), || {
        let calls = mock.calls();
        let set_input_count = calls
            .iter()
            .filter(|call| call.command == "set_input")
            .count();

        if set_input_count == 0 {
            logind.queue_prepare_for_sleep_signal(false);
        }

        set_input_count == 1
            && !runtime.system_marker_path().exists()
            && !runtime.system_sleep_attempt_marker_path().exists()
    });

    assert_eq!(
        mock.calls()
            .iter()
            .filter(|call| call.command == "set_input")
            .count(),
        1
    );
    runtime.assert_system_marker_absent();
    runtime.assert_system_sleep_attempt_marker_absent();

    let (result, output) = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("lifecycle monitor should exit after test event limit");
    lifecycle_thread
        .join()
        .expect("join lifecycle monitor thread");
    result.expect("lifecycle monitor should succeed");

    assert!(!nm_online.invocations().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Using logind system lifecycle source"));
    assert!(output.contains("System resumed from sleep"));
    assert!(output.contains("Session event `after-resume` requests wake restore"));
    assert!(output.contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
    assert!(!output.contains("stopping lifecycle monitor"));
}

#[test]
fn run_lifecycle_monitor_uses_logind_sleep_signal_for_pre_sleep_power_off() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-lifecycle-logind-sleep");
    logind.reset();
    let mock = MockBscpylgtv::new("entrypoint-lifecycle-sleep-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-lifecycle-sleep-wrapper");

    let config = TestConfigFile::new("entrypoint-lifecycle-sleep-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-lifecycle-sleep-runtime");

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_LIFECYCLE_MONITOR_TEST_EVENT_LIMIT", "1");

    let (done_tx, done_rx) = mpsc::channel();
    let lifecycle_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let result = run_command(Command::Lifecycle, &mut output).map_err(|err| err.to_string());
        done_tx
            .send((result, output))
            .expect("send lifecycle result");
    });

    wait_until(Duration::from_secs(4), || {
        let calls = mock.calls();
        let power_off_count = calls
            .iter()
            .filter(|call| call.command == "power_off")
            .count();

        if power_off_count == 0 {
            logind.queue_prepare_for_sleep_signal(true);
        }

        power_off_count == 1 && runtime.system_marker_path().exists()
    });

    assert_eq!(
        mock.calls()
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        vec!["get_input", "power_off"]
    );
    runtime.assert_system_marker_exists();

    let (result, output) = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("lifecycle monitor should exit after test event limit");
    lifecycle_thread
        .join()
        .expect("join lifecycle monitor thread");
    result.expect("lifecycle monitor should succeed");

    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Using logind system lifecycle source"));
    assert!(output.contains("System is preparing for sleep"));
    assert!(output.contains("logind pre-sleep requests TV power-off"));
    assert!(output.contains("Turning off for sleep"));
    assert!(output.contains("Released logind sleep delay inhibitor"));
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }

        let now = Instant::now();
        assert!(now < deadline, "condition was not met within {:?}", timeout);

        let sleep_for = Duration::from_millis(100).min(deadline.saturating_duration_since(now));
        thread::sleep(sleep_for);
    }
}
