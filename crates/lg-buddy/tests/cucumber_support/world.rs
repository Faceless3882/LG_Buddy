use crate::support::{
    ExecutableScript, MockBscpylgtv, MockNmOnline, MockSessionBusIdleMonitor, MockSwayidle,
    MockSystemLogind, RuntimeStateLayout, TestConfigFile, TestEnv,
};
use crate::web_os::{MockWebOsTv, MockWebOsTvSnapshot, MockWebOsVersion, VALID_WEBOS_ACCESS_TOKEN};
use cucumber::World;
use lg_buddy::auth::resolve_bscpylgtv_auth_context_from_env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

#[derive(World, Default)]
pub struct LgBuddyWorld {
    env: Option<TestEnv>,
    config: Option<TestConfigFile>,
    runtime: Option<RuntimeStateLayout>,
    tv: Option<MockBscpylgtv>,
    webos_tv: Option<MockWebOsTv>,
    system_logind: Option<MockSystemLogind>,
    session_bus_idle_monitor: Option<MockSessionBusIdleMonitor>,
    nm_online: Option<MockNmOnline>,
    swayidle: Option<MockSwayidle>,
    path_scripts: Vec<ExecutableScript>,
    config_snapshot: Option<String>,
    systemctl_log_path: Option<PathBuf>,
    command_result: Option<CommandExecution>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecution {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration: std::time::Duration,
}

impl fmt::Debug for LgBuddyWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LgBuddyWorld")
            .field("config", &self.config.is_some())
            .field("runtime", &self.runtime.is_some())
            .field("tv", &self.tv.is_some())
            .field("webos_tv", &self.webos_tv.is_some())
            .field("system_logind", &self.system_logind.is_some())
            .field(
                "session_bus_idle_monitor",
                &self.session_bus_idle_monitor.is_some(),
            )
            .field("nm_online", &self.nm_online.is_some())
            .field("swayidle", &self.swayidle.is_some())
            .field("path_scripts", &self.path_scripts.len())
            .field("config_snapshot", &self.config_snapshot.is_some())
            .field("systemctl_log_path", &self.systemctl_log_path)
            .field("command_result", &self.command_result)
            .finish()
    }
}

impl LgBuddyWorld {
    pub fn create_config(&mut self, input: &str) {
        let config = TestConfigFile::new("cucumber-config");
        config.write_sample(input);
        self.ensure_env().set("LG_BUDDY_CONFIG", config.path());
        self.ensure_env()
            .set("LG_BUDDY_GAMEPAD_ACTIVITY_SOURCE", "disabled");
        self.config = Some(config);
    }

    pub fn create_empty_config_path(&mut self) {
        let config = TestConfigFile::new("cucumber-initial-config");
        self.ensure_env().set("LG_BUDDY_CONFIG", config.path());
        self.config = Some(config);
    }

    pub fn set_screen_restore_policy(&self, policy: &str) {
        self.config
            .as_ref()
            .expect("temporary config should be present")
            .append_line(&format!("screen_restore_policy={policy}"));
    }

    pub fn set_screen_idle_blank(&self, policy: &str) {
        self.config
            .as_ref()
            .expect("temporary config should be present")
            .append_line(&format!("screen_idle_blank={policy}"));
    }

    pub fn set_idle_timeout_secs(&mut self, seconds: u64) {
        self.ensure_env()
            .set("LG_BUDDY_IDLE_TIMEOUT", seconds.to_string());
    }

    pub fn remember_config_contents(&mut self) {
        self.config_snapshot = Some(self.read_config_contents());
    }

    pub fn assert_config_unchanged(&self) {
        assert_eq!(
            self.read_config_contents(),
            self.config_snapshot
                .as_ref()
                .expect("config contents should be remembered")
                .as_str()
        );
    }

    pub fn assert_config_contains(&self, expected: &str) {
        let contents = self.read_config_contents();
        assert!(
            contents.contains(expected),
            "expected config to contain `{expected}`\nconfig was:\n{contents}"
        );
    }

    pub fn assert_config_does_not_contain(&self, unexpected: &str) {
        let contents = self.read_config_contents();
        assert!(
            !contents.contains(unexpected),
            "expected config not to contain `{unexpected}`\nconfig was:\n{contents}"
        );
    }

    pub fn skip_systemd_apply_actions(&mut self) {
        self.ensure_env().set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");
    }

    pub fn install_active_user_screen_service_stub(&mut self) {
        let log_path = self.config().path().with_file_name("systemctl.log");
        let body = format!(
            "#!/bin/sh\n\
log_path={}\n\
printf '%s\\n' \"$*\" >> \"$log_path\"\n\
if [ \"$1\" = \"--user\" ]; then\n\
  case \"$2\" in\n\
    cat)\n\
      cat <<'EOF'\n\
# /home/test/.config/systemd/user/LG_Buddy_screen.service\n\
[Unit]\n\
Description=LG Buddy Screen Monitor Service\n\
EOF\n\
      exit 0\n\
      ;;\n\
    is-active|is-enabled|restart) exit 0 ;;\n\
  esac\n\
fi\n\
exit 1\n",
            shell_quote_path(&log_path)
        );
        let script = ExecutableScript::new(
            "cucumber-settings-systemctl",
            "mock-settings-systemctl",
            &body,
        );
        self.ensure_env().set("LG_BUDDY_SYSTEMCTL", script.path());
        self.ensure_env().remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");
        self.systemctl_log_path = Some(log_path);
        self.path_scripts.push(script);
    }

    pub fn assert_systemctl_invoked_with(&self, expected: &str) {
        let log_path = self
            .systemctl_log_path
            .as_ref()
            .expect("settings systemctl log should be configured");
        let contents = fs::read_to_string(log_path).unwrap_or_default();
        assert!(
            contents.lines().any(|line| line == expected),
            "expected systemctl invocation `{expected}`\nsystemctl log was:\n{contents}"
        );
    }

    pub fn create_runtime(&mut self) {
        let runtime = RuntimeStateLayout::new("cucumber-runtime");
        self.ensure_env()
            .set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
        self.ensure_env()
            .set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
        self.runtime = Some(runtime);
    }

    pub fn create_mock_tv(&mut self) {
        let tv = MockBscpylgtv::new("cucumber-tv");
        let wrapper = tv.command_wrapper("cucumber-tv-wrapper");
        self.ensure_env()
            .set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
        self.path_scripts.push(wrapper);
        self.tv = Some(tv);
    }

    pub fn create_native_webos_tv(&mut self, input: &str, backlight: u8) {
        self.create_native_webos_tv_with_version(
            MockWebOsVersion::WebOs24Version92261,
            input,
            backlight,
        );
    }

    pub fn create_webos26_firmware_43_21_60_tv(&mut self, input: &str, backlight: u8) {
        self.create_native_webos_tv_with_version(
            MockWebOsVersion::WebOs26Firmware432160,
            input,
            backlight,
        );
    }

    fn create_native_webos_tv_with_version(
        &mut self,
        version: MockWebOsVersion,
        input: &str,
        backlight: u8,
    ) {
        if self.config().path().exists() {
            self.config().set_value("tvs_primary_ip", "127.0.0.1");
        }
        let tv = MockWebOsTv::with_version(version, input);
        assert_eq!(
            tv.snapshot().backlight,
            backlight,
            "native WebOS initial brightness must match the hardware-backed TV model"
        );
        self.webos_tv = Some(tv);
    }

    pub fn select_tv_platform(&self, platform: &str) {
        self.config().set_value("tvs_primary_platform", platform);
    }

    pub fn store_native_access_token(&self, access_token: &str) {
        if access_token != VALID_WEBOS_ACCESS_TOKEN {
            self.webos_tv().require_stale_token_pairing();
        }
        let token = lg_buddy::platform_access_token::PlatformAccessToken::new(access_token)
            .expect("valid native access token fixture");
        self.native_access_token_store()
            .persist(&token)
            .expect("persist native access token fixture");
    }

    pub fn store_valid_native_access_token(&self) {
        self.store_native_access_token(VALID_WEBOS_ACCESS_TOKEN);
    }

    pub fn reject_native_pairing(&self) {
        self.webos_tv().reject_pairing();
    }

    pub fn stall_native_tv_response(&self) {
        self.webos_tv().stall_first_tv_response();
    }

    pub fn make_native_restore_ambiguous(&self) {
        self.webos_tv()
            .interrupt_restore_and_ack_input_without_unblanking();
    }

    pub fn configure_system_logind(&mut self, preparing_for_sleep: bool) {
        let logind = MockSystemLogind::new("cucumber-system-logind");
        logind.reset();
        logind.set_preparing_for_sleep(preparing_for_sleep);
        self.ensure_env()
            .set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
        self.system_logind = Some(logind);
    }

    pub fn assert_native_access_token(&self, expected: &str) {
        let stored = self
            .native_access_token_store()
            .load()
            .expect("load native access token")
            .expect("native access token should exist");
        assert_eq!(stored.as_secret_str(), expected);
    }

    pub fn assert_valid_native_access_token(&self) {
        self.assert_native_access_token(VALID_WEBOS_ACCESS_TOKEN);
    }

    pub fn assert_no_native_access_token(&self) {
        assert!(
            self.native_access_token_store()
                .load()
                .expect("load native access token")
                .is_none(),
            "native access token unexpectedly exists"
        );
    }

    pub fn webos_tv(&self) -> &MockWebOsTv {
        self.webos_tv.as_ref().expect("native webOS TV configured")
    }

    pub fn webos_snapshot(&self) -> MockWebOsTvSnapshot {
        self.webos_tv().snapshot()
    }

    pub fn tv(&self) -> &MockBscpylgtv {
        self.tv.as_ref().expect("mock TV configured")
    }

    pub fn tv_mut(&mut self) -> &mut MockBscpylgtv {
        self.tv.as_mut().expect("mock TV configured")
    }

    pub fn config(&self) -> &TestConfigFile {
        self.config.as_ref().expect("config configured")
    }

    pub fn runtime(&self) -> &RuntimeStateLayout {
        self.runtime.as_ref().expect("runtime layout configured")
    }

    pub fn command_result(&self) -> &CommandExecution {
        if let Some(tv) = &self.webos_tv {
            tv.assert_healthy();
        }
        self.command_result
            .as_ref()
            .expect("command result should be present")
    }

    pub fn command_duration(&self) -> std::time::Duration {
        self.command_result().duration
    }

    pub fn create_session_marker(&self) {
        self.runtime().create_session_marker();
    }

    pub fn create_system_marker(&self) {
        self.runtime().create_system_marker();
    }

    pub fn set_auth_key_file_override(&mut self, path: &str) {
        let key_file_path = self
            .config()
            .path()
            .parent()
            .expect("config parent")
            .join(path);
        self.ensure_env()
            .set("LG_BUDDY_BSCPYLGTV_KEY_FILE", &key_file_path);
    }

    pub fn clear_inherited_user_env(&mut self) {
        self.ensure_env().remove("USER");
        self.ensure_env().remove("LOGNAME");
    }

    pub fn assert_tv_calls_match_expected_auth_context(&self) {
        let expected = resolve_bscpylgtv_auth_context_from_env(self.config().path())
            .expect("resolve expected auth context from test config");
        let expected_key_file_path = expected
            .key_file_path()
            .map(|path| path.to_string_lossy().into_owned());
        let expected_user = expected.owner_user().map(ToString::to_string);
        let calls = self.tv().calls();

        assert!(
            !calls.is_empty(),
            "expected at least one TV helper invocation"
        );
        assert!(
            calls
                .iter()
                .all(|call| call.key_file_path == expected_key_file_path),
            "TV helper key paths were: {:?}",
            calls
                .iter()
                .map(|call| call.key_file_path.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            calls.iter().all(|call| call.user == expected_user),
            "TV helper users were: {:?}",
            calls
                .iter()
                .map(|call| call.user.clone())
                .collect::<Vec<_>>()
        );
    }

    pub fn isolate_path(&mut self) {
        self.ensure_env().set("PATH", "");
    }

    pub fn set_backend_override(&mut self, backend: &str) {
        self.ensure_env().set("LG_BUDDY_SCREEN_BACKEND", backend);
    }

    pub fn disable_startup_delays(&mut self) {
        self.ensure_env()
            .set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
        self.ensure_env()
            .set("LG_BUDDY_STARTUP_RETRY_DELAY_SECS", "0");
        self.ensure_env()
            .set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
        self.ensure_env()
            .set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");
    }

    pub fn disable_screen_wake_delays(&mut self) {
        self.ensure_env()
            .set("LG_BUDDY_SCREEN_ON_INITIAL_WAKE_DELAY_SECS", "0");
        self.ensure_env()
            .set("LG_BUDDY_SCREEN_ON_RETRY_DELAY_SECS", "0");
    }

    pub fn disable_sleep_delays(&mut self) {
        self.ensure_env()
            .set("LG_BUDDY_SLEEP_RETRY_DELAY_SECS", "0");
    }

    pub fn install_ping_stub(&mut self, reachable: bool) {
        let status = if reachable { 0 } else { 1 };
        let body = format!("#!/bin/sh\nexit {status}\n");
        let script = ExecutableScript::new("cucumber-ping", "mock-ping", &body);
        self.ensure_env().set("LG_BUDDY_PING", script.path());
        self.path_scripts.push(script);
    }

    pub fn install_brightness_ui_stub(&mut self, selection: Option<u8>) {
        self.ensure_mock_session_bus_idle_monitor()
            .set_notifications_available(true);
        let body = match selection {
            Some(value) => format!(
                "#!/bin/sh\nif [ \"$1\" = \"--scale\" ]; then\n  printf '%s\\n' '{value}'\n  exit 0\nfi\nif [ \"$1\" = \"--error\" ]; then\n  exit 0\nfi\nexit 1\n"
            ),
            None => "#!/bin/sh\nif [ \"$1\" = \"--scale\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"--error\" ]; then\n  exit 0\nfi\nexit 1\n".to_string(),
        };
        let script = ExecutableScript::new("cucumber-zenity", "mock-zenity", &body);
        self.ensure_env().set("LG_BUDDY_ZENITY", script.path());
        self.path_scripts.push(script);
    }

    pub fn install_gnome_shell_stub(&mut self) {
        let bus = self.ensure_mock_session_bus_idle_monitor();
        bus.set_shell_available(true);
        bus.set_screen_saver_available(true);
        bus.set_idle_monitor_available(true);
    }

    pub fn set_gnome_idle_monitor_available(&mut self, value: bool) {
        self.ensure_mock_session_bus_idle_monitor()
            .set_idle_monitor_available(value);
    }

    pub fn gnome_monitor_emit_idle(&mut self) {
        self.ensure_mock_session_bus_idle_monitor()
            .emit_screen_saver_idle();
    }

    pub fn gnome_monitor_emit_active(&mut self) {
        self.ensure_mock_session_bus_idle_monitor()
            .emit_screen_saver_active();
    }

    pub fn gnome_monitor_emit_wake_requested(&mut self) {
        self.ensure_mock_session_bus_idle_monitor()
            .emit_screen_saver_wake_requested();
    }

    pub fn gnome_monitor_emits_no_screen_saver_signals(&mut self) {
        self.ensure_mock_session_bus_idle_monitor()
            .clear_screen_saver_signals();
    }

    pub fn gnome_idle_monitor_reports_idletimes(&mut self, values: &[u64]) {
        let idle_monitor = self.ensure_mock_session_bus_idle_monitor();
        idle_monitor.set_idle_monitor_available(true);
        if let Some(last) = values.last().copied() {
            idle_monitor.set_idle_monitor_idletime(last);
        }
        idle_monitor.set_idle_monitor_idletime_plan(values);
    }

    pub fn gnome_monitor_stays_open_for_secs(&mut self, seconds: f64) {
        self.ensure_env().set(
            "LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS",
            seconds.to_string(),
        );
    }

    pub fn gamepad_activity_occurs_after_secs(&mut self, seconds: f64) {
        self.ensure_env()
            .set("LG_BUDDY_GAMEPAD_ACTIVITY_SOURCE", "synthetic");
        self.ensure_env().set(
            "LG_BUDDY_GAMEPAD_ACTIVITY_TEST_AFTER_SECS",
            seconds.to_string(),
        );
    }

    pub fn install_swayidle_stub(&mut self) {
        if self.swayidle.is_none() {
            let swayidle = MockSwayidle::new("cucumber-swayidle");
            let wrapper = swayidle.command_wrapper("cucumber-swayidle-wrapper");
            self.prepend_path_script(wrapper);
            self.swayidle = Some(swayidle);
        }
    }

    pub fn install_nm_online_stub(&mut self, status: i64) {
        if self.nm_online.is_none() {
            let nm_online = MockNmOnline::new("cucumber-nm-online");
            let wrapper = nm_online.command_wrapper("cucumber-nm-online-wrapper");
            self.ensure_env().set("LG_BUDDY_NM_ONLINE", wrapper.path());
            self.path_scripts.push(wrapper);
            self.nm_online = Some(nm_online);
        }

        self.nm_online
            .as_ref()
            .expect("mock nm-online configured")
            .set_status(status);
    }

    pub fn assert_nm_online_invoked_with(&self, expected_argv: &[&str]) {
        let expected = expected_argv
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let invocations = self
            .nm_online
            .as_ref()
            .expect("mock nm-online configured")
            .invocations();
        assert!(
            invocations
                .iter()
                .any(|invocation| invocation.argv == expected),
            "nm-online invocations were: {:?}",
            invocations
        );
    }

    pub fn swayidle_emits_timeout(&mut self) {
        self.install_swayidle_stub();
        self.swayidle
            .as_ref()
            .expect("mock swayidle configured")
            .queue_timeout_emission();
    }

    pub fn swayidle_emits_resume(&mut self) {
        self.install_swayidle_stub();
        self.swayidle
            .as_ref()
            .expect("mock swayidle configured")
            .queue_resume_emission();
    }

    pub fn install_systemctl_stub(&mut self, reboot_pending: bool) {
        let stdout = if reboot_pending {
            "123 reboot.target start running\n"
        } else {
            ""
        };
        let body = format!("#!/bin/sh\ncat <<'EOF'\n{stdout}EOF\n");
        let script = ExecutableScript::new("cucumber-systemctl", "mock-systemctl", &body);
        self.ensure_env().set("LG_BUDDY_SYSTEMCTL", script.path());
        self.path_scripts.push(script);
    }

    pub fn install_journalctl_stub(&mut self, sleep_requested: bool) {
        let stdout = if sleep_requested {
            "manager: sleep: sleep requested\n"
        } else {
            "manager: unrelated state transition\n"
        };
        let body = format!("#!/bin/sh\ncat <<'EOF'\n{stdout}EOF\n");
        let script = ExecutableScript::new("cucumber-journalctl", "mock-journalctl", &body);
        self.ensure_env().set("LG_BUDDY_JOURNALCTL", script.path());
        self.path_scripts.push(script);
    }

    pub fn run_named_command(&mut self, command_line: &str) {
        let args = command_line.split_whitespace().collect::<Vec<_>>();
        if args == ["monitor"]
            && self.session_bus_idle_monitor.is_some()
            && std::env::var_os("LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS").is_none()
        {
            self.ensure_env()
                .set("LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS", "0.2");
        }
        let started = std::time::Instant::now();
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lg-buddy"))
            .args(args)
            .output()
            .expect("run lg-buddy binary");
        let duration = started.elapsed();

        self.command_result = Some(CommandExecution {
            success: output.status.success(),
            stdout: String::from_utf8(output.stdout).expect("utf8 command output"),
            stderr: String::from_utf8(output.stderr).expect("utf8 command stderr"),
            duration,
        });
    }

    pub fn run_native_initial_configuration(&mut self) {
        self.ensure_env().set("LG_BUDDY_NONINTERACTIVE", "1");
        self.ensure_env().set("LG_BUDDY_TV_IP", "127.0.0.1");
        self.ensure_env()
            .set("LG_BUDDY_TV_MAC", "22:33:44:55:66:77");
        self.ensure_env().set("LG_BUDDY_INPUT", "HDMI_2");
        self.ensure_env().set("LG_BUDDY_TV_PLATFORM", "lg_webos");
        self.ensure_env()
            .set("LG_BUDDY_RUNTIME_BINARY", env!("CARGO_BIN_EXE_lg-buddy"));
        self.ensure_env().set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

        let configure = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("configure.sh");
        let started = std::time::Instant::now();
        let output = ProcessCommand::new(configure)
            .output()
            .expect("run initial configuration");
        let duration = started.elapsed();

        self.command_result = Some(CommandExecution {
            success: output.status.success(),
            stdout: String::from_utf8(output.stdout).expect("utf8 configure output"),
            stderr: String::from_utf8(output.stderr).expect("utf8 configure stderr"),
            duration,
        });
    }

    pub fn assert_tv_input(&self, expected: &str) {
        if let Some(tv) = &self.webos_tv {
            assert_eq!(tv.snapshot().input, expected);
        } else {
            assert_eq!(self.tv().state_snapshot().input, expected);
        }
    }

    pub fn assert_tv_brightness(&self, expected: u8) {
        if let Some(tv) = &self.webos_tv {
            assert_eq!(tv.snapshot().backlight, expected);
        } else {
            assert_eq!(self.tv().state_snapshot().backlight, expected);
        }
    }

    pub fn assert_tv_powered_on(&self, expected: bool) {
        if let Some(tv) = &self.webos_tv {
            assert_eq!(tv.snapshot().power_on, expected);
        } else {
            assert_eq!(self.tv().state_snapshot().power_on, expected);
        }
    }

    pub fn assert_tv_screen_on(&self, expected: bool) {
        if let Some(tv) = &self.webos_tv {
            assert_eq!(tv.snapshot().screen_on, expected);
        } else {
            assert_eq!(self.tv().state_snapshot().screen_on, expected);
        }
    }

    pub fn tv_call_names(&self) -> Vec<String> {
        assert!(
            self.webos_tv.is_none(),
            "native Cucumber scenarios must assert product outcomes, not mock call labels"
        );
        self.tv()
            .calls()
            .into_iter()
            .map(|call| call.command)
            .collect()
    }

    fn native_access_token_store(
        &self,
    ) -> lg_buddy::platform_access_token::PlatformAccessTokenStore {
        let owner = lg_buddy::auth::resolve_config_owner(self.config().path())
            .expect("resolve native access token owner");
        lg_buddy::platform_access_token::PlatformAccessTokenStore::for_primary_profile(
            self.config().path(),
            owner,
        )
        .expect("construct native access token store")
    }

    fn prepend_path_script(&mut self, script: ExecutableScript) {
        let dir = script
            .path()
            .parent()
            .expect("script path should have a parent")
            .to_path_buf();
        self.prepend_path_dir(&dir);
        self.path_scripts.push(script);
    }

    fn prepend_path_dir(&mut self, dir: &Path) {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut combined = Vec::new();
        combined.push(dir.to_path_buf());
        combined.extend(std::env::split_paths(&current));
        let joined = std::env::join_paths(combined).expect("join PATH entries");
        self.ensure_env().set("PATH", joined);
    }

    fn ensure_env(&mut self) -> &mut TestEnv {
        if self.env.is_none() {
            let mut env = TestEnv::new();
            env.set(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/tmp/lg-buddy-nonexistent-session-bus",
            );
            self.env = Some(env);
        }

        self.env.as_mut().expect("test env configured")
    }

    fn ensure_mock_session_bus_idle_monitor(&mut self) -> &mut MockSessionBusIdleMonitor {
        if self.session_bus_idle_monitor.is_none() {
            let session_bus_idle_monitor =
                MockSessionBusIdleMonitor::new("cucumber-session-bus-idle-monitor");
            self.ensure_env().set(
                "DBUS_SESSION_BUS_ADDRESS",
                session_bus_idle_monitor.address(),
            );
            self.session_bus_idle_monitor = Some(session_bus_idle_monitor);
        }

        self.session_bus_idle_monitor
            .as_mut()
            .expect("mock session-bus idle monitor configured")
    }

    fn read_config_contents(&self) -> String {
        fs::read_to_string(self.config().path()).expect("read temporary config")
    }
}

fn shell_quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}
