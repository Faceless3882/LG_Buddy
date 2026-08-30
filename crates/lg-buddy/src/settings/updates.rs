use super::{
    ApplyStrategy, EnumSettingType, ServiceController, SettingDefinition, SettingMutability,
    SettingType, SettingValue, SettingsApplyOutcome, SettingsChange, SettingsError,
    UserServiceState, UserUnitEnableOutcome, EMPTY_ALIASES, EMPTY_STORAGE_KEYS,
    READ_WRITE_OPERATIONS,
};

const CHECK_TIMER_NAME: &str = "LG_Buddy_update_check.timer";
const AUTO_CHECK_VALUES: &[&str] = &["enabled", "disabled"];
const CHANNEL_VALUES: &[&str] = &["stable", "prerelease"];

pub(super) const AUTO_CHECK: SettingDefinition = SettingDefinition {
    key: "updates.auto_check",
    storage_key: "updates_auto_check",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: AUTO_CHECK_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("enabled")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::ManageUpdateCheckTimer,
    description: "Automatic background update checks and update notifications.",
};

pub(super) const CHANNEL: SettingDefinition = SettingDefinition {
    key: "updates.channel",
    storage_key: "updates_channel",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: CHANNEL_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("stable")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RuntimePolicyOnly,
    description: "Release channel used by all update operations.",
};

pub(super) fn apply_check_timer<C: ServiceController>(
    service_controller: &C,
    change: &SettingsChange,
) -> Result<SettingsApplyOutcome, SettingsError> {
    if service_controller.systemd_actions_disabled() {
        return Ok(SettingsApplyOutcome::Skipped {
            reason: "skipped systemd apply because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1".to_string(),
        });
    }

    let enabled = match change.mutation().new_value()?.as_enum() {
        Some("enabled") => true,
        Some("disabled") => false,
        _ => {
            return Err(SettingsError::Apply {
                message: "updates.auto_check resolved to an invalid value".to_string(),
            });
        }
    };

    match service_controller.user_service_state(CHECK_TIMER_NAME)? {
        UserServiceState::Missing => Ok(SettingsApplyOutcome::NotInstalled {
            service: CHECK_TIMER_NAME,
        }),
        UserServiceState::InactiveDisabled => {
            if enabled {
                let outcome = service_controller.enable_start_user_unit(CHECK_TIMER_NAME)?;
                Ok(enable_outcome(CHECK_TIMER_NAME, outcome))
            } else {
                service_controller.disable_stop_user_unit(CHECK_TIMER_NAME)?;
                Ok(SettingsApplyOutcome::DisabledStopped {
                    unit: CHECK_TIMER_NAME,
                })
            }
        }
        UserServiceState::ActiveOrEnabled => {
            if enabled {
                let outcome = service_controller.enable_start_user_unit(CHECK_TIMER_NAME)?;
                Ok(enable_outcome(CHECK_TIMER_NAME, outcome))
            } else {
                service_controller.disable_stop_user_unit(CHECK_TIMER_NAME)?;
                Ok(SettingsApplyOutcome::DisabledStopped {
                    unit: CHECK_TIMER_NAME,
                })
            }
        }
    }
}

fn enable_outcome(unit: &'static str, outcome: UserUnitEnableOutcome) -> SettingsApplyOutcome {
    match outcome {
        UserUnitEnableOutcome::Enabled => SettingsApplyOutcome::Enabled { unit },
        UserUnitEnableOutcome::EnabledStarted => SettingsApplyOutcome::EnabledStarted { unit },
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{unique_test_path, FakeServiceController};
    use super::super::*;
    use std::fs;

    #[test]
    fn settings_runner_disables_update_timer_when_auto_check_is_disabled() {
        let path = unique_test_path("disable-update-checks");
        fs::write(&path, "updates_auto_check=enabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let fake_service = FakeServiceController::active_or_enabled();
        let disables = fake_service.disables.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "updates.auto_check".to_string(),
                    value: "disabled".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "updates_auto_check=disabled\n"
        );
        assert_eq!(disables.get(), 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("updates.auto_check=disabled (saved to "));
        assert!(output.contains("apply: disabled and stopped LG_Buddy_update_check.timer\n"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_enables_update_timer_when_auto_check_is_enabled() {
        let path = unique_test_path("enable-update-checks");
        fs::write(&path, "updates_auto_check=disabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let fake_service = FakeServiceController::inactive_disabled();
        let enables = fake_service.enables.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "updates.auto_check".to_string(),
                    value: "enabled".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "updates_auto_check=enabled\n"
        );
        assert_eq!(enables.get(), 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("updates.auto_check=enabled (saved to "));
        assert!(output.contains("apply: enabled and started LG_Buddy_update_check.timer\n"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_reports_missing_update_timer_after_persisting_auto_check() {
        let path = unique_test_path("missing-update-check-timer");
        fs::write(&path, "updates_auto_check=enabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "updates.auto_check".to_string(),
                    value: "disabled".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "updates_auto_check=disabled\n"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: LG_Buddy_update_check.timer is not installed"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_skips_update_timer_apply_when_systemd_actions_are_disabled() {
        let path = unique_test_path("skip-update-timer-apply");
        fs::write(&path, "updates_auto_check=enabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let mut fake_service = FakeServiceController::active_or_enabled();
        fake_service.skip_actions = true;
        let disables = fake_service.disables.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "updates.auto_check".to_string(),
                    value: "disabled".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "updates_auto_check=disabled\n"
        );
        assert_eq!(disables.get(), 0);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: skipped systemd apply"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_reports_update_timer_apply_failure_after_persisting_value() {
        let path = unique_test_path("update-timer-apply-fail");
        fs::write(&path, "updates_auto_check=enabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::failing_unit_action()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "updates.auto_check".to_string(),
                    value: "disabled".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "updates_auto_check=disabled\n"
        );
        assert!(matches!(err, SettingsError::ApplyAfterPersist { .. }));
        assert!(err.to_string().contains("was saved"));
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }
}
