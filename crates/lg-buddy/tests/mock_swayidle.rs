mod support;

use lg_buddy::session::{IdleTimeoutSource, SessionBackend, SessionBackendCapabilities};
use lg_buddy::swayidle::{SwayidleBackend, SwayidleProbe, SystemSwayidleProbe};
use std::process::Command;
use support::{MockSwayidle, MockSwayidleEvent, TestEnv};

#[test]
fn system_probe_detects_systemd_hook_surface_from_mock_help() {
    let mut env = TestEnv::new();
    let mock = MockSwayidle::new("mock-swayidle-systemd-help");
    let wrapper = mock.command_wrapper("mock-swayidle-systemd-help-wrapper");
    env.set("PATH", wrapper.path().parent().expect("wrapper dir"));

    let probe = SystemSwayidleProbe;

    assert!(SwayidleProbe::swayidle_available(&probe));
    assert!(SwayidleProbe::systemd_hooks_available(&probe));
}

#[test]
fn system_probe_treats_minimal_help_surface_as_no_systemd_hook_support() {
    let mut env = TestEnv::new();
    let mock = MockSwayidle::new("mock-swayidle-minimal-help");
    mock.disable_systemd_hooks_in_help();
    let wrapper = mock.command_wrapper("mock-swayidle-minimal-help-wrapper");
    env.set("PATH", wrapper.path().parent().expect("wrapper dir"));

    let probe = SystemSwayidleProbe;

    assert!(SwayidleProbe::swayidle_available(&probe));
    assert!(!SwayidleProbe::systemd_hooks_available(&probe));
}

#[test]
fn default_swayidle_backend_reports_capabilities_from_mock_probe_surface() {
    let mut env = TestEnv::new();
    let mock = MockSwayidle::new("mock-swayidle-default-backend");
    let wrapper = mock.command_wrapper("mock-swayidle-default-backend-wrapper");
    env.set("PATH", wrapper.path().parent().expect("wrapper dir"));

    let backend = SwayidleBackend::default();

    assert_eq!(
        backend.capabilities().expect("backend should be available"),
        SessionBackendCapabilities {
            idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
            wake_requested: false,
            before_sleep: true,
            after_resume: true,
            lock_unlock: true,
            early_user_activity: false,
        }
    );
}

#[test]
fn mock_records_expected_swayidle_event_configuration_shape() {
    let _env = TestEnv::new();
    let mock = MockSwayidle::new("mock-swayidle-config-shape");
    let wrapper = mock.command_wrapper("mock-swayidle-config-shape-wrapper");

    let output = Command::new(wrapper.path())
        .args([
            "-w",
            "-d",
            "-S",
            "seat0",
            "timeout",
            "300",
            "screen off",
            "resume",
            "screen on",
            "before-sleep",
            "pre-sleep",
            "after-resume",
            "post-resume",
            "lock",
            "lock-session",
            "unlock",
            "unlock-session",
            "idlehint",
            "60",
        ])
        .output()
        .expect("run mock swayidle");

    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        mock.invocations(),
        vec![support::MockSwayidleInvocation {
            argv: vec![
                "-w".to_string(),
                "-d".to_string(),
                "-S".to_string(),
                "seat0".to_string(),
                "timeout".to_string(),
                "300".to_string(),
                "screen off".to_string(),
                "resume".to_string(),
                "screen on".to_string(),
                "before-sleep".to_string(),
                "pre-sleep".to_string(),
                "after-resume".to_string(),
                "post-resume".to_string(),
                "lock".to_string(),
                "lock-session".to_string(),
                "unlock".to_string(),
                "unlock-session".to_string(),
                "idlehint".to_string(),
                "60".to_string(),
            ],
            wait: true,
            debug: true,
            config_path: None,
            seat: Some("seat0".to_string()),
            events: vec![
                MockSwayidleEvent::Timeout {
                    timeout: 300,
                    command: "screen off".to_string(),
                    resume: Some("screen on".to_string()),
                },
                MockSwayidleEvent::BeforeSleep {
                    command: "pre-sleep".to_string(),
                },
                MockSwayidleEvent::AfterResume {
                    command: "post-resume".to_string(),
                },
                MockSwayidleEvent::Lock {
                    command: "lock-session".to_string(),
                },
                MockSwayidleEvent::Unlock {
                    command: "unlock-session".to_string(),
                },
                MockSwayidleEvent::Idlehint { timeout: 60 },
            ],
        }]
    );
}
