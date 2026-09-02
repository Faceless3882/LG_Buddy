use crate::backend::{resolve_backend_from_system, BackendResolution, SWAYIDLE_DEPRECATION_NOTICE};
use crate::config::{ScreenBackend, DEFAULT_IDLE_TIMEOUT, MAX_IDLE_TIMEOUT};

use super::{
    ApplyStrategy, EnumSettingType, IntegerSettingType, ServiceController, SettingAlias,
    SettingDefinition, SettingMutability, SettingType, SettingValue, SettingsApplyOutcome,
    SettingsCommand, SettingsError, UserServiceState, EMPTY_ALIASES, EMPTY_STORAGE_KEYS,
    READ_WRITE_OPERATIONS,
};

const SERVICE_NAME: &str = "LG_Buddy_screen.service";
const BACKEND_VALUES: &[&str] = &["auto", "gnome", "wayland", "swayidle"];
const IDLE_BLANK_VALUES: &[&str] = &["enabled", "disabled"];
const RESTORE_POLICY_VALUES: &[&str] = &["conservative", "aggressive"];
const RESTORE_POLICY_ALIASES: &[SettingAlias] = &[SettingAlias {
    from: "marker_only",
    to: "conservative",
}];

pub(super) const BACKEND: SettingDefinition = SettingDefinition {
    key: "screen.backend",
    storage_key: "screen_backend",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: BACKEND_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("auto")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RestartUserScreenService,
    description: "Screen backend selection for user-session blanking and restore behavior.",
};

pub(super) const IDLE_BLANK: SettingDefinition = SettingDefinition {
    key: "screen.idle_blank",
    storage_key: "screen_idle_blank",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: IDLE_BLANK_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("enabled")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RestartUserScreenService,
    description: "Idle-driven blanking and restore behavior for the configured screen.",
};

pub(super) const IDLE_TIMEOUT: SettingDefinition = SettingDefinition {
    key: "screen.idle_timeout",
    storage_key: "screen_idle_timeout",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Integer(IntegerSettingType {
        min: 1,
        max: MAX_IDLE_TIMEOUT as i64,
    }),
    default_value: Some(SettingValue::Integer(DEFAULT_IDLE_TIMEOUT as i64)),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RestartUserScreenService,
    description: "Idle timeout in seconds before LG Buddy blanks the configured screen.",
};

pub(super) const RESTORE_POLICY: SettingDefinition = SettingDefinition {
    key: "screen.restore_policy",
    storage_key: "screen_restore_policy",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: RESTORE_POLICY_VALUES,
        aliases: RESTORE_POLICY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("conservative")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RestartUserScreenService,
    description: "Screen restore policy after LG Buddy blanks the configured screen.",
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BackendPresentation {
    Raw,
    Resolved(Result<BackendResolution, String>),
}

pub(super) fn presentation_for_command(
    command: &SettingsCommand,
    configured: Option<&str>,
) -> BackendPresentation {
    let describes_backend = match command {
        SettingsCommand::Describe(None) => true,
        SettingsCommand::Describe(Some(key)) => key == "screen.backend",
        _ => false,
    };

    let Some(configured) = configured.and_then(|value| value.parse::<ScreenBackend>().ok()) else {
        return BackendPresentation::Raw;
    };
    if !describes_backend {
        return BackendPresentation::Raw;
    }

    BackendPresentation::Resolved(
        resolve_backend_from_system(configured).map_err(|err| err.to_string()),
    )
}

pub(super) fn format_backend_choice(value: &str) -> String {
    if value == ScreenBackend::Swayidle.as_str() {
        format!("{value} (deprecated compatibility backend)")
    } else {
        value.to_string()
    }
}

pub(super) fn resolution_details(
    configured: &str,
    presentation: &BackendPresentation,
) -> Option<(String, String)> {
    match presentation {
        BackendPresentation::Raw => None,
        BackendPresentation::Resolved(Ok(resolution)) => Some((
            resolution.backend().as_str().to_string(),
            resolution
                .fallback_reason()
                .unwrap_or_else(|| {
                    if configured == ScreenBackend::Auto.as_str() {
                        "none; preferred backend is available"
                    } else {
                        "none; explicit selection does not fall back"
                    }
                })
                .to_string(),
        )),
        BackendPresentation::Resolved(Err(reason)) => {
            Some(("unavailable".to_string(), reason.clone()))
        }
    }
}

pub(super) fn deprecation_notice(configured: &str) -> Option<&'static str> {
    (configured == ScreenBackend::Swayidle.as_str()).then_some(SWAYIDLE_DEPRECATION_NOTICE)
}

pub(super) fn apply_service_restart<C: ServiceController>(
    service_controller: &C,
) -> Result<SettingsApplyOutcome, SettingsError> {
    if service_controller.systemd_actions_disabled() {
        return Ok(SettingsApplyOutcome::Skipped {
            reason: "skipped systemd apply because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1".to_string(),
        });
    }

    match service_controller.user_service_state(SERVICE_NAME)? {
        UserServiceState::Missing => Ok(SettingsApplyOutcome::NotInstalled {
            service: SERVICE_NAME,
        }),
        UserServiceState::InactiveDisabled => Ok(SettingsApplyOutcome::InactiveDisabled {
            service: SERVICE_NAME,
        }),
        UserServiceState::ActiveOrEnabled => {
            service_controller.restart_user_service(SERVICE_NAME)?;
            Ok(SettingsApplyOutcome::Restarted {
                service: SERVICE_NAME,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{unique_test_path, FakeServiceController};
    use super::super::*;
    use super::*;
    use std::fs;

    #[test]
    fn settings_runner_describe_includes_metadata_and_operations() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_restore_policy=marker_only\n")
                .into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.restore_policy".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.restore_policy\n"));
        assert!(output.contains("  storage key: screen_restore_policy\n"));
        assert!(output.contains("  type: enum\n"));
        assert!(output.contains("  current: conservative\n"));
        assert!(output.contains("  source: config.env\n"));
        assert!(output.contains("  default: conservative\n"));
        assert!(output.contains("  mutability: read-write\n"));
        assert!(output.contains("  supported operations: get, describe, set, unset\n"));
        assert!(output.contains("  allowed values: conservative, aggressive\n"));
        assert!(output.contains("  aliases: marker_only -> conservative\n"));
        assert!(output.contains("  apply: restart-user-screen-service\n"));
    }

    #[test]
    fn settings_runner_describe_distinguishes_auto_resolution_without_changing_get() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "screen_backend=auto\n").into_store();
        let runner = SettingsCommandRunner::new(store).with_screen_backend_presentation(
            BackendPresentation::Resolved(Ok(BackendResolution::selected(
                ScreenBackend::Gnome,
                None,
            ))),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.backend".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("  current: auto\n"));
        assert!(output.contains("  resolved backend: gnome\n"));
        assert!(output.contains("  fallback reason: none; preferred backend is available\n"));
        assert!(output.contains(
            "  allowed values: auto, gnome, wayland, swayidle (deprecated compatibility backend)\n"
        ));

        let mut raw_output = Vec::new();
        runner
            .run(
                SettingsCommand::Get("screen.backend".to_string()),
                &mut raw_output,
            )
            .unwrap();
        assert_eq!(String::from_utf8(raw_output).unwrap(), "auto\n");
    }

    #[test]
    fn settings_runner_describe_reports_when_auto_has_no_available_backend() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "screen_backend=auto\n").into_store();
        let runner = SettingsCommandRunner::new(store).with_screen_backend_presentation(
            BackendPresentation::Resolved(
                Err("native Wayland protocol is unavailable".to_string()),
            ),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.backend".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("  current: auto\n"));
        assert!(output.contains("  resolved backend: unavailable\n"));
        assert!(output.contains("  fallback reason: native Wayland protocol is unavailable\n"));
        assert!(output.contains(
            "  allowed values: auto, gnome, wayland, swayidle (deprecated compatibility backend)\n"
        ));
    }

    #[test]
    fn settings_runner_marks_explicit_swayidle_as_deprecated() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_backend=swayidle\n").into_store();
        let runner = SettingsCommandRunner::new(store).with_screen_backend_presentation(
            BackendPresentation::Resolved(Ok(BackendResolution::selected(
                ScreenBackend::Swayidle,
                None,
            ))),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.backend".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("  current: swayidle (deprecated compatibility backend)\n"));
        assert!(output.contains("  resolved backend: swayidle\n"));
        assert!(output.contains("  fallback reason: none; explicit selection does not fall back\n"));
        assert!(output.contains("  deprecation: swayidle is a deprecated compatibility backend planned for removal in LG Buddy 2.0.0; use auto or wayland.\n"));
    }

    #[test]
    fn settings_runner_reports_an_explicit_wayland_capability_limit() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_backend=wayland\n").into_store();
        let runner = SettingsCommandRunner::new(store).with_screen_backend_presentation(
            BackendPresentation::Resolved(Err(
                "backend `wayland` is unavailable: the compositor advertises ext_idle_notifier_v1 version 1; version 2 or newer is required"
                    .to_string(),
            )),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.backend".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("  current: wayland\n"));
        assert!(output.contains("  resolved backend: unavailable\n"));
        assert!(output.contains("ext_idle_notifier_v1 version 1; version 2 or newer is required\n"));
    }

    #[test]
    fn settings_runner_sets_value_and_restarts_active_screen_service() {
        let path = unique_test_path("set");
        fs::write(
            &path,
            "\
tv_ip=192.168.1.42
screen_backend=swayidle # keep backend comment
screen_idle_timeout=300
",
        )
        .unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let fake_service = FakeServiceController::active_or_enabled();
        let restarts = fake_service.restarts.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(restarts.get(), 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
tv_ip=192.168.1.42
screen_backend=gnome # keep backend comment
screen_idle_timeout=300
"
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.backend=gnome (saved to "));
        assert!(output.contains("apply: restarted LG_Buddy_screen.service\n"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_clamps_idle_timeout_above_max_when_setting() {
        let path = unique_test_path("set-clamped-idle-timeout");
        fs::write(&path, "screen_idle_timeout=300\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "screen.idle_timeout".to_string(),
                    value: "86401".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("screen_idle_timeout={MAX_IDLE_TIMEOUT}\n")
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(&format!("screen.idle_timeout={MAX_IDLE_TIMEOUT} ")));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_reports_apply_failure_after_persisting_value() {
        let path = unique_test_path("apply-fail");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::failing_restart()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(fs::read_to_string(&path).unwrap(), "screen_backend=gnome\n");
        assert!(matches!(err, SettingsError::ApplyAfterPersist { .. }));
        assert!(err.to_string().contains("was saved"));
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_store_preserves_invalid_optional_values() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=not-a-backend
            screen_idle_blank=not-a-policy
            screen_idle_timeout=not-a-number
            screen_restore_policy=not-a-policy
            ",
        )
        .into_store();

        let backend = store.effective_by_name("screen.backend").unwrap();
        assert_eq!(backend.value(), None);
        assert_eq!(backend.source(), SettingSource::InvalidConfigEnv);
        assert_eq!(backend.invalid_value(), Some("not-a-backend"));

        let idle_blank = store.effective_by_name("screen.idle_blank").unwrap();
        assert_eq!(idle_blank.value(), None);
        assert_eq!(idle_blank.source(), SettingSource::InvalidConfigEnv);
        assert_eq!(idle_blank.invalid_value(), Some("not-a-policy"));

        let idle_timeout = store.effective_by_name("screen.idle_timeout").unwrap();
        assert_eq!(idle_timeout.value(), None);
        assert_eq!(idle_timeout.source(), SettingSource::InvalidConfigEnv);
        assert_eq!(idle_timeout.invalid_value(), Some("not-a-number"));

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();
        assert_eq!(restore_policy.value(), None);
        assert_eq!(restore_policy.source(), SettingSource::InvalidConfigEnv);
        assert_eq!(restore_policy.invalid_value(), Some("not-a-policy"));
    }

    #[test]
    fn settings_store_clamps_idle_timeout_above_max() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_idle_timeout=86401\n").into_store();

        let idle_timeout = store.effective_by_name("screen.idle_timeout").unwrap();

        assert_eq!(
            idle_timeout.value(),
            Some(SettingValue::Integer(MAX_IDLE_TIMEOUT as i64))
        );
        assert_eq!(idle_timeout.source(), SettingSource::ConfigEnv);
    }

    #[test]
    fn settings_store_canonicalizes_valid_alias_values_from_config_env() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_restore_policy=marker_only\n")
                .into_store();

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();

        assert_eq!(
            restore_policy.value(),
            Some(SettingValue::Enum("conservative"))
        );
        assert_eq!(restore_policy.source(), SettingSource::ConfigEnv);
    }

    #[test]
    fn screen_backend_values_are_validated() {
        let definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();

        for value in ["auto", "gnome", "wayland", "swayidle"] {
            assert_eq!(definition.parse_value(value), Ok(SettingValue::Enum(value)));
        }

        assert!(matches!(
            definition.parse_value("kde"),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn screen_restore_policy_accepts_legacy_alias() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.restore_policy")
            .unwrap();

        assert_eq!(
            definition.parse_value("conservative"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("marker_only"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("aggressive"),
            Ok(SettingValue::Enum("aggressive"))
        );
    }

    #[test]
    fn integer_values_are_range_checked() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.parse_value("1"), Ok(SettingValue::Integer(1)));
        assert_eq!(
            definition.parse_value("86400"),
            Ok(SettingValue::Integer(86_400))
        );
        assert_eq!(
            definition.parse_value("86401"),
            Ok(SettingValue::Integer(86_400))
        );
        assert_eq!(
            definition.parse_value("18446744073709551615"),
            Ok(SettingValue::Integer(86_400))
        );

        for value in ["0", "-1", "abc"] {
            assert!(
                matches!(
                    definition.parse_value(value),
                    Err(SettingsError::InvalidValue { .. })
                ),
                "expected invalid idle timeout for `{value}`"
            );
        }
    }
}
