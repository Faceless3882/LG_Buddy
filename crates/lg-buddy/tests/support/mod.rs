use dbus::arg::{PropMap, Variant as DbusVariant};
use dbus::blocking::Connection as DbusConnection;
use dbus::channel::{MatchingReceiver, Sender as DbusSender};
use dbus::Message as DbusMessage;
use dbus_crossroads::{Crossroads, MethodErr};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{self, Command as ProcessCommand};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub struct MockBscpylgtv {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockBscpylgtv {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "power_on": true,
            "screen_on": true,
            "input": "HDMI_3",
            "backlight": 50,
            "volume": 20,
            "muted": false,
            "plan": {},
            "calls": [],
        }));
        mock
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn command_path(&self) -> &'static str {
        "python3"
    }

    pub fn command_args(&self) -> Vec<String> {
        vec![
            Self::script_path().to_string_lossy().into_owned(),
            "--state".to_string(),
            self.state_path.to_string_lossy().into_owned(),
        ]
    }

    pub fn set_power_on(&self, value: bool) {
        self.patch_state(json!({ "power_on": value }));
    }

    pub fn set_screen_on(&self, value: bool) {
        self.patch_state(json!({ "screen_on": value }));
    }

    pub fn set_input(&self, value: &str) {
        self.patch_state(json!({ "input": value }));
    }

    pub fn set_backlight(&self, value: u64) {
        self.patch_state(json!({ "backlight": value }));
    }

    pub fn set_volume(&self, value: i16) {
        self.patch_state(json!({ "volume": value }));
    }

    pub fn set_muted(&self, value: bool) {
        self.patch_state(json!({ "muted": value }));
    }

    pub fn queue_success(&self, command: &str, stdout: &str) {
        self.queue_step(
            command,
            json!({
                "result": "success",
                "stdout": stdout,
            }),
        );
    }

    pub fn queue_error(&self, command: &str, status: i64, stderr: &str) {
        self.queue_step(
            command,
            json!({
                "result": "error",
                "status": status,
                "stderr": stderr,
            }),
        );
    }

    pub fn queue_active_screen_error(&self, command: &str) {
        self.queue_step(
            command,
            json!({
                "result": "active_screen_error",
            }),
        );
    }

    pub fn queue_powered_off_error(&self, command: &str) {
        self.queue_step(
            command,
            json!({
                "result": "powered_off_error",
            }),
        );
    }

    pub fn queue_set_input_wake_success(&self) {
        self.queue_step(
            "set_input",
            json!({
                "result": "success",
                "stdout": "{'returnValue': True}\n",
                "state_update": {
                    "power_on": true
                }
            }),
        );
    }

    pub fn queue_set_input_ack_without_screen_on(&self) {
        self.queue_step(
            "set_input",
            json!({
                "result": "success",
                "stdout": "{'returnValue': True}\n",
                "state_update": {
                    "power_on": true,
                    "screen_on": false
                }
            }),
        );
    }

    pub fn calls(&self) -> Vec<MockInvocation> {
        self.load_state()
            .get("calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockInvocation::from_value)
            .collect()
    }

    pub fn state_snapshot(&self) -> MockStateSnapshot {
        let state = self.load_state();
        MockStateSnapshot {
            power_on: state
                .get("power_on")
                .and_then(Value::as_bool)
                .expect("mock state power_on bool"),
            screen_on: state
                .get("screen_on")
                .and_then(Value::as_bool)
                .expect("mock state screen_on bool"),
            input: state
                .get("input")
                .and_then(Value::as_str)
                .expect("mock state input string")
                .to_string(),
            backlight: state
                .get("backlight")
                .and_then(Value::as_u64)
                .expect("mock state backlight integer") as u8,
            volume: state
                .get("volume")
                .and_then(Value::as_i64)
                .expect("mock state volume integer") as i16,
            muted: state
                .get("muted")
                .and_then(Value::as_bool)
                .expect("mock state muted bool"),
        }
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "mock-bscpylgtvcommand", &body)
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_bscpylgtvcommand.py")
    }

    fn queue_step(&self, command: &str, step: Value) {
        let mut state = self.load_state();
        let plan = state
            .as_object_mut()
            .expect("mock state object")
            .entry("plan")
            .or_insert_with(|| Value::Object(Map::new()));
        let plan = plan.as_object_mut().expect("plan object");
        let steps = plan
            .entry(command.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        steps.as_array_mut().expect("plan command array").push(step);
        self.save_state(state);
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct MockSwayidle {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockSwayidle {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "help_mode": "systemd",
            "emissions": [],
            "invocations": [],
        }));
        mock
    }

    pub fn command_path(&self) -> &'static str {
        "python3"
    }

    pub fn command_args(&self) -> Vec<String> {
        vec![
            Self::script_path().to_string_lossy().into_owned(),
            "--state".to_string(),
            self.state_path.to_string_lossy().into_owned(),
        ]
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "swayidle", &body)
    }

    pub fn disable_systemd_hooks_in_help(&self) {
        self.patch_state(json!({ "help_mode": "minimal" }));
    }

    pub fn queue_timeout_emission(&self) {
        self.queue_emission("timeout");
    }

    pub fn queue_resume_emission(&self) {
        self.queue_emission("resume");
    }

    pub fn queue_before_sleep_emission(&self) {
        self.queue_emission("before-sleep");
    }

    pub fn queue_after_resume_emission(&self) {
        self.queue_emission("after-resume");
    }

    pub fn invocations(&self) -> Vec<MockSwayidleInvocation> {
        self.load_state()
            .get("invocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockSwayidleInvocation::from_value)
            .collect()
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_swayidle.py")
    }

    fn queue_emission(&self, emission: &str) {
        let mut state = self.load_state();
        let emissions = state
            .as_object_mut()
            .expect("mock swayidle state object")
            .entry("emissions")
            .or_insert_with(|| Value::Array(Vec::new()));
        emissions
            .as_array_mut()
            .expect("emissions array")
            .push(Value::String(emission.to_string()));
        self.save_state(state);
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct MockSessionBusIdleMonitor {
    _temp_dir: TestDir,
    address: String,
    daemon_pid: i32,
    state: Arc<Mutex<MockSessionBusIdleMonitorState>>,
    stop: Arc<AtomicBool>,
    service_thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct MockSessionBusIdleMonitorState {
    shell_available: bool,
    screen_saver_available: bool,
    idle_monitor_available: bool,
    notifications_available: bool,
    default_idletime: u64,
    idletime_plan: VecDeque<u64>,
    screen_saver_signals: VecDeque<MockScreenSaverSignal>,
    client_ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockScreenSaverSignal {
    ActiveChanged(bool),
    WakeUpScreen,
}

#[allow(dead_code)]
impl MockSessionBusIdleMonitor {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let (address, daemon_pid) = start_private_session_bus();
        let state = Arc::new(Mutex::new(MockSessionBusIdleMonitorState {
            default_idletime: 1500,
            ..MockSessionBusIdleMonitorState::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let service_thread = Some(spawn_mock_idle_monitor_service(
            address.clone(),
            Arc::clone(&state),
            Arc::clone(&stop),
            ready_tx,
        ));

        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mock session-bus idle monitor should become ready");

        Self {
            _temp_dir: temp_dir,
            address,
            daemon_pid,
            state,
            stop,
            service_thread,
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn set_shell_available(&self, value: bool) {
        self.patch_state(|state| state.shell_available = value);
        wait_for_mock_bus_name_sync();
    }

    pub fn set_screen_saver_available(&self, value: bool) {
        self.patch_state(|state| state.screen_saver_available = value);
        wait_for_mock_bus_name_sync();
    }

    pub fn set_idle_monitor_available(&self, value: bool) {
        self.patch_state(|state| {
            state.idle_monitor_available = value;
            if !value {
                state.client_ready = false;
            }
        });
        wait_for_mock_bus_name_sync();
    }

    pub fn set_notifications_available(&self, value: bool) {
        self.patch_state(|state| {
            state.notifications_available = value;
        });
        wait_for_mock_bus_name_sync();
    }

    pub fn set_idle_monitor_idletime(&self, value: u64) {
        self.patch_state(|state| state.default_idletime = value);
    }

    pub fn set_idle_monitor_idletime_plan(&self, values: &[u64]) {
        self.patch_state(|state| {
            state.idletime_plan = values.iter().copied().collect();
        });
    }

    pub fn queue_idle_monitor_idletime(&self, value: u64) {
        self.patch_state(|state| {
            state.idletime_plan.push_back(value);
        });
    }

    pub fn emit_screen_saver_idle(&self) {
        self.patch_state(|state| {
            state
                .screen_saver_signals
                .push_back(MockScreenSaverSignal::ActiveChanged(true));
        });
    }

    pub fn emit_screen_saver_active(&self) {
        self.patch_state(|state| {
            state
                .screen_saver_signals
                .push_back(MockScreenSaverSignal::ActiveChanged(false));
        });
    }

    pub fn emit_screen_saver_wake_requested(&self) {
        self.patch_state(|state| {
            state
                .screen_saver_signals
                .push_back(MockScreenSaverSignal::WakeUpScreen);
        });
    }

    pub fn clear_screen_saver_signals(&self) {
        self.patch_state(|state| {
            state.screen_saver_signals.clear();
        });
    }

    fn patch_state<F>(&self, f: F)
    where
        F: FnOnce(&mut MockSessionBusIdleMonitorState),
    {
        let mut state = self
            .state
            .lock()
            .expect("mock session-bus idle monitor state lock");
        f(&mut state);
    }
}

impl Drop for MockSessionBusIdleMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(service_thread) = self.service_thread.take() {
            let _ = service_thread.join();
        }

        unsafe {
            libc::kill(self.daemon_pid, libc::SIGTERM);
        }
    }
}

#[allow(dead_code)]
pub struct MockSystemLogind {
    _temp_dir: TestDir,
    address: String,
    state: Arc<Mutex<MockSystemLogindState>>,
}

#[derive(Debug, Default)]
struct MockSystemLogindState {
    preparing_for_sleep: bool,
    prepare_for_sleep_signals: VecDeque<bool>,
}

#[allow(dead_code)]
impl MockSystemLogind {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let global = global_mock_system_logind();

        Self {
            _temp_dir: temp_dir,
            address: global.address.clone(),
            state: Arc::clone(&global.state),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn reset(&self) {
        self.patch_state(|state| {
            state.preparing_for_sleep = false;
            state.prepare_for_sleep_signals.clear();
        });
    }

    pub fn set_preparing_for_sleep(&self, value: bool) {
        self.patch_state(|state| state.preparing_for_sleep = value);
    }

    pub fn queue_prepare_for_sleep_signal(&self, value: bool) {
        self.patch_state(|state| state.prepare_for_sleep_signals.push_back(value));
    }

    fn patch_state<F>(&self, f: F)
    where
        F: FnOnce(&mut MockSystemLogindState),
    {
        let mut state = self.state.lock().expect("mock logind state lock");
        f(&mut state);
    }
}

struct GlobalMockSystemLogind {
    address: String,
    state: Arc<Mutex<MockSystemLogindState>>,
}

fn global_mock_system_logind() -> &'static GlobalMockSystemLogind {
    static GLOBAL: OnceLock<GlobalMockSystemLogind> = OnceLock::new();

    GLOBAL.get_or_init(|| {
        let (address, daemon_pid) = start_private_session_bus();
        MOCK_SYSTEM_LOGIND_DAEMON_PID.store(daemon_pid, Ordering::SeqCst);
        unsafe {
            libc::atexit(kill_mock_system_logind_daemon);
        }

        let state = Arc::new(Mutex::new(MockSystemLogindState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let _service_thread = spawn_mock_logind_service(
            address.clone(),
            Arc::clone(&state),
            Arc::clone(&stop),
            ready_tx,
        );

        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mock logind service should become ready");

        GlobalMockSystemLogind { address, state }
    })
}

static MOCK_SYSTEM_LOGIND_DAEMON_PID: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(0);

extern "C" fn kill_mock_system_logind_daemon() {
    let pid = MOCK_SYSTEM_LOGIND_DAEMON_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

fn start_private_session_bus() -> (String, i32) {
    let output = ProcessCommand::new(dbus_daemon_path())
        .args([
            "--session",
            "--fork",
            "--print-address=1",
            "--print-pid=1",
            "--nopidfile",
        ])
        .output()
        .expect("spawn private dbus-daemon");
    assert!(
        output.status.success(),
        "dbus-daemon failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("dbus-daemon stdout utf8");
    let mut lines = stdout.lines();
    let address = lines
        .next()
        .expect("dbus-daemon address line")
        .trim()
        .to_string();
    let daemon_pid = lines
        .next()
        .expect("dbus-daemon pid line")
        .trim()
        .parse::<i32>()
        .expect("dbus-daemon pid integer");
    (address, daemon_pid)
}

fn spawn_mock_idle_monitor_service(
    address: String,
    state: Arc<Mutex<MockSessionBusIdleMonitorState>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<()>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let connection = DbusConnection::new_address(&address)
            .expect("connect mock idle monitor service to private session bus");

        let mut crossroads = Crossroads::new();
        let idle_monitor_state = Arc::clone(&state);
        let iface = crossroads.register("org.gnome.Mutter.IdleMonitor", move |builder| {
            let state = Arc::clone(&idle_monitor_state);
            builder.method("GetIdletime", (), ("idletime",), move |_, _, ()| {
                let mut state = state
                    .lock()
                    .expect("mock session-bus idle monitor state lock");
                state.client_ready = true;
                let value = state
                    .idletime_plan
                    .pop_front()
                    .unwrap_or(state.default_idletime);
                Ok((value,))
            });
        });
        let notifications_iface =
            crossroads.register("org.freedesktop.Notifications", move |builder| {
                builder.method("GetCapabilities", (), ("capabilities",), move |_, _, ()| {
                    Ok((vec!["actions".to_string()],))
                });

                builder.method(
                    "Notify",
                    (
                        "app_name",
                        "replaces_id",
                        "app_icon",
                        "summary",
                        "body",
                        "actions",
                        "hints",
                        "expire_timeout",
                    ),
                    ("id",),
                    move |_,
                          _,
                          (
                        _app_name,
                        _replaces_id,
                        _app_icon,
                        _summary,
                        _body,
                        _actions,
                        _hints,
                        _expire_timeout,
                    ): (
                        String,
                        u32,
                        String,
                        String,
                        String,
                        Vec<String>,
                        PropMap,
                        i32,
                    )| { Ok((1_u32,)) },
                );
            });
        crossroads.insert("/org/gnome/Mutter/IdleMonitor/Core", &[iface], ());
        crossroads.insert("/org/freedesktop/Notifications", &[notifications_iface], ());

        let shared_crossroads = Arc::new(Mutex::new(crossroads));
        let crossroads_receiver = Arc::clone(&shared_crossroads);
        connection.start_receive(
            dbus::message::MatchRule::new_method_call(),
            Box::new(move |message, conn| {
                crossroads_receiver
                    .lock()
                    .expect("mock idle monitor crossroads lock")
                    .handle_message(message, conn)
                    .expect("handle mock idle monitor message");
                true
            }),
        );

        let _ = ready.send(());
        let mut owned_names = MockOwnedBusNames::default();
        while !stop.load(Ordering::SeqCst) {
            let _ = connection.process(Duration::from_millis(50));
            sync_mock_bus_names(&connection, &state, &mut owned_names);
            emit_queued_mock_screen_saver_signal(&connection, &state);
        }
    })
}

fn spawn_mock_logind_service(
    address: String,
    state: Arc<Mutex<MockSystemLogindState>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<()>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let connection = DbusConnection::new_address(&address)
            .expect("connect mock logind service to private system bus");
        connection
            .request_name("org.freedesktop.login1", false, true, false)
            .expect("request mock logind bus name");

        let mut crossroads = Crossroads::new();
        let properties_state = Arc::clone(&state);
        let properties_iface =
            crossroads.register("org.freedesktop.DBus.Properties", move |builder| {
                let state = Arc::clone(&properties_state);
                builder.method(
                    "Get",
                    ("interface_name", "property_name"),
                    ("value",),
                    move |_, _, (interface_name, property_name): (String, String)| {
                        if interface_name != "org.freedesktop.login1.Manager" {
                            return Err(MethodErr::no_interface(&interface_name));
                        }

                        if property_name != "PreparingForSleep" {
                            return Err(MethodErr::no_property(&property_name));
                        }

                        let preparing_for_sleep = state
                            .lock()
                            .expect("mock logind state lock")
                            .preparing_for_sleep;
                        Ok((DbusVariant(preparing_for_sleep),))
                    },
                );
            });
        let manager_iface = crossroads.register("org.freedesktop.login1.Manager", move |builder| {
            builder.method(
                "Inhibit",
                ("what", "who", "why", "mode"),
                ("fd",),
                move |_, _, (_what, _who, _why, _mode): (String, String, String, String)| {
                    let mut pipe_fds = [0; 2];
                    let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
                    if pipe_result != 0 {
                        return Err(MethodErr::failed("mock logind could not create pipe"));
                    }

                    unsafe {
                        libc::close(pipe_fds[1]);
                    }
                    let fd = unsafe { dbus::arg::OwnedFd::from_raw_fd(pipe_fds[0]) };
                    Ok((fd,))
                },
            );
        });
        crossroads.insert(
            "/org/freedesktop/login1",
            &[properties_iface, manager_iface],
            (),
        );

        let shared_crossroads = Arc::new(Mutex::new(crossroads));
        let crossroads_receiver = Arc::clone(&shared_crossroads);
        connection.start_receive(
            dbus::message::MatchRule::new_method_call(),
            Box::new(move |message, conn| {
                crossroads_receiver
                    .lock()
                    .expect("mock logind crossroads lock")
                    .handle_message(message, conn)
                    .expect("handle mock logind message");
                true
            }),
        );

        let _ = ready.send(());
        while !stop.load(Ordering::SeqCst) {
            let _ = connection.process(Duration::from_millis(50));
            emit_queued_mock_logind_signal(&connection, &state);
        }
    })
}

fn emit_queued_mock_logind_signal(
    connection: &DbusConnection,
    state: &Arc<Mutex<MockSystemLogindState>>,
) {
    let signal = {
        let mut state = state.lock().expect("mock logind state lock");
        state.prepare_for_sleep_signals.pop_front()
    };
    let Some(preparing_for_sleep) = signal else {
        return;
    };

    let message = DbusMessage::new_signal(
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        "PrepareForSleep",
    )
    .expect("create mock PrepareForSleep signal")
    .append1(preparing_for_sleep);

    let _ = connection.send(message);
}

#[derive(Debug, Default)]
struct MockOwnedBusNames {
    shell: bool,
    screen_saver: bool,
    idle_monitor: bool,
    notifications: bool,
}

fn sync_mock_bus_names(
    connection: &DbusConnection,
    state: &Arc<Mutex<MockSessionBusIdleMonitorState>>,
    owned_names: &mut MockOwnedBusNames,
) {
    let (want_shell, want_screen_saver, want_idle_monitor, want_notifications) = {
        let state = state
            .lock()
            .expect("mock session-bus idle monitor state lock");
        (
            state.shell_available,
            state.screen_saver_available,
            state.idle_monitor_available,
            state.notifications_available,
        )
    };

    sync_mock_bus_name(
        connection,
        "org.gnome.Shell",
        want_shell,
        &mut owned_names.shell,
    );
    sync_mock_bus_name(
        connection,
        "org.gnome.ScreenSaver",
        want_screen_saver,
        &mut owned_names.screen_saver,
    );
    sync_mock_bus_name(
        connection,
        "org.gnome.Mutter.IdleMonitor",
        want_idle_monitor,
        &mut owned_names.idle_monitor,
    );
    sync_mock_bus_name(
        connection,
        "org.freedesktop.Notifications",
        want_notifications,
        &mut owned_names.notifications,
    );
}

fn sync_mock_bus_name(connection: &DbusConnection, name: &str, wanted: bool, owned: &mut bool) {
    if wanted == *owned {
        return;
    }

    if wanted {
        connection
            .request_name(name, false, true, false)
            .unwrap_or_else(|err| panic!("request mock bus name `{name}`: {err}"));
    } else {
        connection
            .release_name(name)
            .unwrap_or_else(|err| panic!("release mock bus name `{name}`: {err}"));
    }

    *owned = wanted;
}

fn wait_for_mock_bus_name_sync() {
    thread::sleep(Duration::from_millis(100));
}

fn emit_queued_mock_screen_saver_signal(
    connection: &DbusConnection,
    state: &Arc<Mutex<MockSessionBusIdleMonitorState>>,
) {
    let signal = {
        let mut state = state
            .lock()
            .expect("mock session-bus idle monitor state lock");
        if !state.client_ready || !state.screen_saver_available {
            return;
        }
        state.screen_saver_signals.pop_front()
    };
    let Some(signal) = signal else {
        return;
    };

    let message = match signal {
        MockScreenSaverSignal::ActiveChanged(active) => DbusMessage::new_signal(
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "ActiveChanged",
        )
        .expect("create mock ActiveChanged signal")
        .append1(active),
        MockScreenSaverSignal::WakeUpScreen => DbusMessage::new_signal(
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "WakeUpScreen",
        )
        .expect("create mock WakeUpScreen signal"),
    };

    let _ = connection.send(message);
}

#[allow(dead_code)]
pub struct MockNmOnline {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockNmOnline {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "status": 0,
            "invocations": [],
        }));
        mock
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "mock-nm-online", &body)
    }

    pub fn set_status(&self, status: i64) {
        self.patch_state(json!({ "status": status }));
    }

    pub fn invocations(&self) -> Vec<MockNmOnlineInvocation> {
        self.load_state()
            .get("invocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockNmOnlineInvocation::from_value)
            .collect()
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_nm_online.py")
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    original_values: Vec<(OsString, Option<OsString>)>,
}

#[allow(dead_code)]
impl TestEnv {
    pub fn new() -> Self {
        Self {
            _guard: env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            original_values: Vec::new(),
        }
    }

    pub fn set<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        self.remember_original_value(&key);
        env::set_var(&key, value.as_ref());
    }

    pub fn remove<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        self.remember_original_value(&key);
        env::remove_var(&key);
    }

    fn remember_original_value(&mut self, key: &OsStr) {
        if self.original_values.iter().any(|(saved, _)| saved == key) {
            return;
        }

        self.original_values
            .push((key.to_os_string(), env::var_os(key)));
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.original_values.iter().rev() {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}

#[allow(dead_code)]
pub struct TestConfigFile {
    _temp_dir: TestDir,
    path: PathBuf,
}

#[allow(dead_code)]
impl TestConfigFile {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let path = temp_dir.path().join("config.env");
        Self {
            _temp_dir: temp_dir,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_contents(&self, contents: &str) {
        fs::write(&self.path, contents).expect("write temp config");
    }

    pub fn append_line(&self, line: &str) {
        let mut contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
            Err(err) => panic!("read temp config: {err}"),
        };
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(line);
        contents.push('\n');
        self.write_contents(&contents);
    }

    pub fn set_value(&self, key: &str, value: &str) {
        let contents = fs::read_to_string(&self.path).expect("read temp config");
        let prefix = format!("{key}=");
        let mut found = false;
        let mut updated = contents
            .lines()
            .map(|line| {
                if line.starts_with(&prefix) {
                    found = true;
                    format!("{key}={value}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>();
        if !found {
            updated.push(format!("{key}={value}"));
        }
        self.write_contents(&format!("{}\n", updated.join("\n")));
    }

    pub fn write_sample(&self, input: &str) {
        self.write_contents(&sample_config_contents(input));
    }
}

#[allow(dead_code)]
pub fn sample_config_contents(input: &str) -> String {
    format!(
        "tvs_primary_ip=192.0.2.42\n\
tvs_primary_mac=aa:bb:cc:dd:ee:ff\n\
tvs_primary_input={input}\n\
screen_backend=auto\n\
screen_idle_timeout=300\n"
    )
}

#[allow(dead_code)]
pub struct RuntimeStateLayout {
    _temp_dir: TestDir,
    root: PathBuf,
}

#[allow(dead_code)]
impl RuntimeStateLayout {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let root = temp_dir.path().to_path_buf();
        Self {
            _temp_dir: temp_dir,
            root,
        }
    }

    pub fn session_dir(&self) -> PathBuf {
        self.root.join("session")
    }

    pub fn system_dir(&self) -> PathBuf {
        self.root.join("system")
    }

    pub fn session_marker_path(&self) -> PathBuf {
        self.session_dir().join("screen_off_by_us")
    }

    pub fn system_marker_path(&self) -> PathBuf {
        self.system_dir().join("screen_off_by_us")
    }

    pub fn system_sleep_attempt_marker_path(&self) -> PathBuf {
        self.system_dir().join("system_sleep_attempted")
    }

    pub fn create_session_marker(&self) {
        self.create_marker(&self.session_marker_path());
    }

    pub fn create_system_marker(&self) {
        self.create_marker(&self.system_marker_path());
    }

    pub fn create_system_sleep_attempt_marker(&self) {
        self.create_marker(&self.system_sleep_attempt_marker_path());
    }

    pub fn assert_session_marker_exists(&self) {
        assert!(
            self.session_marker_path().is_file(),
            "expected session marker at {}",
            self.session_marker_path().display()
        );
    }

    pub fn assert_session_marker_absent(&self) {
        assert!(
            !self.session_marker_path().exists(),
            "did not expect session marker at {}",
            self.session_marker_path().display()
        );
    }

    pub fn assert_system_marker_exists(&self) {
        assert!(
            self.system_marker_path().is_file(),
            "expected system marker at {}",
            self.system_marker_path().display()
        );
    }

    pub fn assert_system_marker_absent(&self) {
        assert!(
            !self.system_marker_path().exists(),
            "did not expect system marker at {}",
            self.system_marker_path().display()
        );
    }

    pub fn assert_system_sleep_attempt_marker_exists(&self) {
        assert!(
            self.system_sleep_attempt_marker_path().is_file(),
            "expected system sleep attempt marker at {}",
            self.system_sleep_attempt_marker_path().display()
        );
    }

    pub fn assert_system_sleep_attempt_marker_absent(&self) {
        assert!(
            !self.system_sleep_attempt_marker_path().exists(),
            "did not expect system sleep attempt marker at {}",
            self.system_sleep_attempt_marker_path().display()
        );
    }

    fn create_marker(&self, path: &Path) {
        let parent = path.parent().expect("marker parent");
        fs::create_dir_all(parent).expect("create marker parent");
        fs::write(path, []).expect("write marker");
    }
}

#[allow(dead_code)]
pub struct ExecutableScript {
    _temp_dir: TestDir,
    path: PathBuf,
}

#[allow(dead_code)]
impl ExecutableScript {
    pub fn new(label: &str, file_name: &str, body: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let path = temp_dir.path().join(file_name);
        fs::write(&path, body).expect("write executable script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("set script permissions");
        }

        Self {
            _temp_dir: temp_dir,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockInvocation {
    pub tv_ip: String,
    pub command: String,
    pub args: Vec<String>,
    pub key_file_path: Option<String>,
    pub user: Option<String>,
}

impl MockInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock invocation object");
        Self {
            tv_ip: object
                .get("tv_ip")
                .and_then(Value::as_str)
                .expect("invocation tv_ip string")
                .to_string(),
            command: object
                .get("command")
                .and_then(Value::as_str)
                .expect("invocation command string")
                .to_string(),
            args: object
                .get("args")
                .and_then(Value::as_array)
                .expect("invocation args array")
                .iter()
                .map(|value| value.as_str().expect("invocation arg string").to_string())
                .collect(),
            key_file_path: object
                .get("key_file_path")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            user: object
                .get("user")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockStateSnapshot {
    pub power_on: bool,
    pub screen_on: bool,
    pub input: String,
    pub backlight: u8,
    pub volume: i16,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockSwayidleInvocation {
    pub argv: Vec<String>,
    pub wait: bool,
    pub debug: bool,
    pub config_path: Option<String>,
    pub seat: Option<String>,
    pub events: Vec<MockSwayidleEvent>,
}

impl MockSwayidleInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock swayidle invocation object");
        Self {
            argv: object
                .get("argv")
                .and_then(Value::as_array)
                .expect("invocation argv array")
                .iter()
                .map(|value| value.as_str().expect("argv string").to_string())
                .collect(),
            wait: object
                .get("wait")
                .and_then(Value::as_bool)
                .expect("invocation wait bool"),
            debug: object
                .get("debug")
                .and_then(Value::as_bool)
                .expect("invocation debug bool"),
            config_path: object
                .get("config_path")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            seat: object
                .get("seat")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            events: object
                .get("events")
                .and_then(Value::as_array)
                .expect("invocation events array")
                .iter()
                .map(MockSwayidleEvent::from_value)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockNmOnlineInvocation {
    pub argv: Vec<String>,
}

impl MockNmOnlineInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock nm-online invocation object");
        Self {
            argv: object
                .get("argv")
                .and_then(Value::as_array)
                .expect("invocation argv array")
                .iter()
                .map(|value| value.as_str().expect("argv string").to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MockSwayidleEvent {
    Timeout {
        timeout: u64,
        command: String,
        resume: Option<String>,
    },
    BeforeSleep {
        command: String,
    },
    AfterResume {
        command: String,
    },
    Lock {
        command: String,
    },
    Unlock {
        command: String,
    },
    Idlehint {
        timeout: u64,
    },
}

impl MockSwayidleEvent {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock swayidle event object");
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .expect("event kind string");

        match kind {
            "timeout" => Self::Timeout {
                timeout: object
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .expect("timeout value"),
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("timeout command")
                    .to_string(),
                resume: object
                    .get("resume")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            },
            "before-sleep" => Self::BeforeSleep {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("before-sleep command")
                    .to_string(),
            },
            "after-resume" => Self::AfterResume {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("after-resume command")
                    .to_string(),
            },
            "lock" => Self::Lock {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("lock command")
                    .to_string(),
            },
            "unlock" => Self::Unlock {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("unlock command")
                    .to_string(),
            },
            "idlehint" => Self::Idlehint {
                timeout: object
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .expect("idlehint timeout"),
            },
            other => panic!("unsupported mock swayidle event kind `{other}`"),
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-{label}-{}-{timestamp}-{unique}",
            process::id()
        ));

        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[allow(dead_code)]
pub fn prime_isolated_path_dependencies() {
    let _ = python3_path();
    let _ = dbus_daemon_path();
}

fn python3_path() -> PathBuf {
    static PYTHON3_PATH: OnceLock<PathBuf> = OnceLock::new();

    PYTHON3_PATH
        .get_or_init(|| {
            find_command_in_path("python3")
                .or_else(|| find_command_in_path("python"))
                .or_else(find_python3_in_standard_locations)
                .unwrap_or_else(|| PathBuf::from("python3"))
        })
        .clone()
}

fn dbus_daemon_path() -> PathBuf {
    static DBUS_DAEMON_PATH: OnceLock<PathBuf> = OnceLock::new();

    DBUS_DAEMON_PATH
        .get_or_init(|| {
            find_command_in_path("dbus-daemon")
                .or_else(find_dbus_daemon_in_standard_locations)
                .unwrap_or_else(|| PathBuf::from("dbus-daemon"))
        })
        .clone()
}

fn find_command_in_path(command: &str) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(command);
        return path.is_file().then_some(path);
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        candidate.is_file().then_some(candidate)
    })
}

fn find_python3_in_standard_locations() -> Option<PathBuf> {
    [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
        "/usr/local/bin/python",
        "/bin/python",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.is_file())
}

fn find_dbus_daemon_in_standard_locations() -> Option<PathBuf> {
    [
        "/usr/bin/dbus-daemon",
        "/usr/local/bin/dbus-daemon",
        "/bin/dbus-daemon",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.is_file())
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy().replace('\'', "'\"'\"'");
    format!("'{rendered}'")
}
