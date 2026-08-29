use super::{
    ApplyStrategy, EnumSettingType, SettingDefinition, SettingMutability, SettingType,
    SettingValue, EMPTY_ALIASES, EMPTY_STORAGE_KEYS, READ_WRITE_OPERATIONS,
};

const SLEEP_WAKE_POLICY_VALUES: &[&str] = &["enabled", "disabled"];

pub(super) const SLEEP_WAKE_POLICY: SettingDefinition = SettingDefinition {
    key: "system.sleep_wake_policy",
    storage_key: "system_sleep_wake_policy",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: SLEEP_WAKE_POLICY_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("enabled")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::RuntimePolicyOnly,
    description: "System sleep and wake policy for lifecycle hooks.",
};

#[cfg(test)]
mod tests {
    use super::super::tests::{unique_test_path, FakeServiceController};
    use super::super::*;
    use std::fs;

    #[test]
    fn settings_runner_sets_lifecycle_policy_without_service_restart() {
        let path = unique_test_path("lifecycle-policy");
        fs::write(&path, "system_sleep_wake_policy=disabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let fake_service = FakeServiceController::active_or_enabled();
        let restarts = fake_service.restarts.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "system.sleep_wake_policy".to_string(),
                    value: "enabled".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "system_sleep_wake_policy=enabled\n"
        );
        assert_eq!(restarts.get(), 0);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("system.sleep_wake_policy=enabled (saved to "));
        assert!(output.contains("apply: no runtime apply action required\n"));

        let _ = fs::remove_file(path);
    }
}
