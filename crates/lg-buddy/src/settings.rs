use std::io;

mod command;
mod formatter;
mod model;
mod screen;
mod service;
mod store;
mod system;
mod tv;
mod updates;

pub use command::{SettingsCommand, SettingsParseError};
pub use formatter::SettingsFormatter;
pub use model::{
    ApplyStrategy, EnumSettingType, IntegerSettingType, SettingAlias, SettingDefinition,
    SettingKey, SettingMutability, SettingOperation, SettingType, SettingValue, SettingsError,
    SettingsRegistry,
};
pub use service::{
    ServiceController, SettingsApplyOutcome, SystemdUserServiceController, UserServiceState,
    UserUnitEnableOutcome,
};
pub use store::{
    ConfigEnvEditor, ConfigEnvReader, ConfigPathResolver, EffectiveSetting, SettingSource,
    SettingsChange, SettingsMutation, SettingsMutationAction, SettingsStore,
};
pub use tv::{PlatformPreflight, WebOsPlatformPreflight};

#[cfg(test)]
use formatter::format_effective_value;
use store::persist_settings_mutation;

const READ_WRITE_OPERATIONS: &[SettingOperation] = &[
    SettingOperation::Get,
    SettingOperation::Describe,
    SettingOperation::Set,
    SettingOperation::Unset,
];
const READ_SET_OPERATIONS: &[SettingOperation] = &[
    SettingOperation::Get,
    SettingOperation::Describe,
    SettingOperation::Set,
];
const EMPTY_ALIASES: &[SettingAlias] = &[];
const EMPTY_STORAGE_KEYS: &[&str] = &[];

const SETTING_DEFINITIONS: &[SettingDefinition] = &[
    tv::IP,
    tv::MAC,
    tv::INPUT,
    tv::PLATFORM,
    screen::BACKEND,
    screen::IDLE_BLANK,
    screen::IDLE_TIMEOUT,
    screen::RESTORE_POLICY,
    system::SLEEP_WAKE_POLICY,
    updates::AUTO_CHECK,
    updates::CHANNEL,
];

pub static SETTINGS_REGISTRY: SettingsRegistry = SettingsRegistry {
    definitions: SETTING_DEFINITIONS,
};

#[derive(Debug, Clone)]
pub struct SettingsApplier<C = SystemdUserServiceController> {
    service_controller: C,
}

impl SettingsApplier<SystemdUserServiceController> {
    pub fn from_env() -> Self {
        Self {
            service_controller: SystemdUserServiceController::from_env(),
        }
    }
}

impl<C: ServiceController> SettingsApplier<C> {
    pub fn new(service_controller: C) -> Self {
        Self { service_controller }
    }

    pub fn apply(&self, change: &SettingsChange) -> Result<SettingsApplyOutcome, SettingsError> {
        match change.mutation().definition().apply_strategy() {
            ApplyStrategy::RestartUserScreenService => {
                screen::apply_service_restart(&self.service_controller)
            }
            ApplyStrategy::ManageUpdateCheckTimer => {
                updates::apply_check_timer(&self.service_controller, change)
            }
            ApplyStrategy::RuntimePolicyOnly | ApplyStrategy::NoRuntimeApplyRequired => {
                Ok(SettingsApplyOutcome::NoActionRequired)
            }
        }
    }
}

#[derive(Debug)]
pub struct SettingsCommandRunner<C = SystemdUserServiceController, P = WebOsPlatformPreflight> {
    store: SettingsStore,
    formatter: SettingsFormatter,
    applier: SettingsApplier<C>,
    preflight: P,
    screen_backend: screen::BackendPresentation,
}

impl SettingsCommandRunner<SystemdUserServiceController, WebOsPlatformPreflight> {
    pub fn new(store: SettingsStore) -> Self {
        Self::with_applier(store, SettingsApplier::from_env())
    }
}

impl<C: ServiceController> SettingsCommandRunner<C, WebOsPlatformPreflight> {
    pub fn with_applier(store: SettingsStore, applier: SettingsApplier<C>) -> Self {
        Self::with_applier_and_preflight(store, applier, WebOsPlatformPreflight)
    }
}

impl<C: ServiceController, P: PlatformPreflight> SettingsCommandRunner<C, P> {
    pub fn with_applier_and_preflight(
        store: SettingsStore,
        applier: SettingsApplier<C>,
        preflight: P,
    ) -> Self {
        Self {
            store,
            formatter: SettingsFormatter,
            applier,
            preflight,
            screen_backend: screen::BackendPresentation::Raw,
        }
    }

    fn with_screen_backend_presentation(
        mut self,
        presentation: screen::BackendPresentation,
    ) -> Self {
        self.screen_backend = presentation;
        self
    }

    pub fn run<W: io::Write>(
        &self,
        command: SettingsCommand,
        writer: &mut W,
    ) -> Result<(), SettingsError> {
        match command {
            SettingsCommand::List => {
                let settings = self.store.all_effective();
                self.formatter.write_list(writer, &settings)
            }
            SettingsCommand::Describe(key) => match key {
                Some(key) => {
                    let setting = self.store.effective_by_name(&key)?;
                    self.formatter.write_describe_with_backend(
                        writer,
                        &[setting],
                        &self.screen_backend,
                    )
                }
                None => {
                    let settings = self.store.all_effective();
                    self.formatter.write_describe_with_backend(
                        writer,
                        &settings,
                        &self.screen_backend,
                    )
                }
            },
            SettingsCommand::Get(key) => {
                let setting = self.store.effective_by_name(&key)?;
                self.formatter.write_get(writer, setting)
            }
            SettingsCommand::Set { key, value } => {
                let mutation = SettingsMutation::set(&self.store, &key, &value)?;
                tv::preflight_if_required(&self.store, &self.preflight, &mutation, writer)?;
                let change = persist_settings_mutation(self.store.path(), mutation)?;
                let apply = self.apply_after_persist(&change)?;
                self.formatter.write_change(writer, &change, &apply)
            }
            SettingsCommand::Unset(key) => {
                let mutation = SettingsMutation::unset(&self.store, &key)?;
                let change = persist_settings_mutation(self.store.path(), mutation)?;
                let apply = self.apply_after_persist(&change)?;
                self.formatter.write_change(writer, &change, &apply)
            }
        }
    }

    fn apply_after_persist(
        &self,
        change: &SettingsChange,
    ) -> Result<SettingsApplyOutcome, SettingsError> {
        self.applier
            .apply(change)
            .map_err(|err| SettingsError::ApplyAfterPersist {
                key: change.mutation().key_name().to_string(),
                path: change.path().to_path_buf(),
                message: err.to_string(),
            })
    }
}

pub fn run_settings_command<W: io::Write>(
    command: SettingsCommand,
    writer: &mut W,
) -> Result<(), SettingsError> {
    let store = SettingsStore::load_from_env()?;
    let configured_backend = store
        .effective_by_name("screen.backend")
        .ok()
        .and_then(|setting| setting.value())
        .map(|value| value.to_string());
    let presentation = screen::presentation_for_command(&command, configured_backend.as_deref());
    let runner = SettingsCommandRunner::new(store);
    let runner = runner.with_screen_backend_presentation(presentation);
    runner.run(command, writer)
}

#[cfg(test)]
mod tests {
    use super::{
        format_effective_value, ApplyStrategy, ConfigEnvReader, ConfigPathResolver,
        ServiceController, SettingKey, SettingMutability, SettingOperation, SettingSource,
        SettingType, SettingValue, SettingsApplier, SettingsCommand, SettingsCommandRunner,
        SettingsError, SettingsParseError, SettingsStore, UserServiceState, UserUnitEnableOutcome,
        SETTINGS_REGISTRY,
    };
    use crate::config::ConfigPathSources;
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_metadata_is_internally_valid() {
        SETTINGS_REGISTRY.validate().unwrap();
    }

    #[test]
    fn registry_contains_initial_settings() {
        let keys: Vec<&str> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| definition.key_name())
            .collect();

        assert_eq!(
            keys,
            vec![
                "tv.ip",
                "tv.mac",
                "tv.input",
                "tv.platform",
                "screen.backend",
                "screen.idle_blank",
                "screen.idle_timeout",
                "screen.restore_policy",
                "system.sleep_wake_policy",
                "updates.auto_check",
                "updates.channel",
            ]
        );
    }

    #[test]
    fn registry_maps_public_keys_to_storage_keys() {
        let mappings: Vec<(&str, &str)> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| (definition.key_name(), definition.storage_key()))
            .collect();

        assert_eq!(
            mappings,
            vec![
                ("tv.ip", "tvs_primary_ip"),
                ("tv.mac", "tvs_primary_mac"),
                ("tv.input", "tvs_primary_input"),
                ("tv.platform", "tvs_primary_platform"),
                ("screen.backend", "screen_backend"),
                ("screen.idle_blank", "screen_idle_blank"),
                ("screen.idle_timeout", "screen_idle_timeout"),
                ("screen.restore_policy", "screen_restore_policy"),
                ("system.sleep_wake_policy", "system_sleep_wake_policy"),
                ("updates.auto_check", "updates_auto_check"),
                ("updates.channel", "updates_channel"),
            ]
        );
    }

    #[test]
    fn registry_public_contract_is_pinned() {
        let contracts: Vec<String> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(setting_definition_contract)
            .collect();

        assert_eq!(
            contracts,
            vec![
                "tv.ip | storage=tvs_primary_ip | fallbacks=tv_ip | type=ipv4 | default=required | mutability=read-write | ops=get,describe,set | apply=no-runtime-apply-required | description=IPv4 address of the primary configured TV.",
                "tv.mac | storage=tvs_primary_mac | fallbacks=tv_mac | type=mac-address | default=required | mutability=read-write | ops=get,describe,set | apply=no-runtime-apply-required | description=MAC address of the primary configured TV for Wake-on-LAN.",
                "tv.input | storage=tvs_primary_input | fallbacks=input | type=enum values=HDMI_1,HDMI_2,HDMI_3,HDMI_4 aliases=(none) | default=required | mutability=read-write | ops=get,describe,set | apply=no-runtime-apply-required | description=HDMI input used by the primary configured TV.",
                "tv.platform | storage=tvs_primary_platform | fallbacks=(none) | type=enum values=bscpylgtv,lg_webos aliases=(none) | default=bscpylgtv | mutability=read-write | ops=get,describe,set,unset | apply=no-runtime-apply-required | description=Control platform for the primary configured TV.",
                "screen.backend | storage=screen_backend | fallbacks=(none) | type=enum values=auto,gnome,wayland,swayidle aliases=(none) | default=auto | mutability=read-write | ops=get,describe,set,unset | apply=restart-user-screen-service | description=Screen backend selection for user-session blanking and restore behavior.",
                "screen.idle_blank | storage=screen_idle_blank | fallbacks=(none) | type=enum values=enabled,disabled aliases=(none) | default=enabled | mutability=read-write | ops=get,describe,set,unset | apply=restart-user-screen-service | description=Idle-driven blanking and restore behavior for the configured screen.",
                "screen.idle_timeout | storage=screen_idle_timeout | fallbacks=(none) | type=integer range=1..=86400 | default=300 | mutability=read-write | ops=get,describe,set,unset | apply=restart-user-screen-service | description=Idle timeout in seconds before LG Buddy blanks the configured screen.",
                "screen.restore_policy | storage=screen_restore_policy | fallbacks=(none) | type=enum values=conservative,aggressive aliases=marker_only->conservative | default=conservative | mutability=read-write | ops=get,describe,set,unset | apply=restart-user-screen-service | description=Screen restore policy after LG Buddy blanks the configured screen.",
                "system.sleep_wake_policy | storage=system_sleep_wake_policy | fallbacks=(none) | type=enum values=enabled,disabled aliases=(none) | default=enabled | mutability=read-write | ops=get,describe,set,unset | apply=runtime-policy-only | description=System sleep and wake policy for lifecycle hooks.",
                "updates.auto_check | storage=updates_auto_check | fallbacks=(none) | type=enum values=enabled,disabled aliases=(none) | default=enabled | mutability=read-write | ops=get,describe,set,unset | apply=manage-update-check-timer | description=Automatic background update checks and update notifications.",
                "updates.channel | storage=updates_channel | fallbacks=(none) | type=enum values=stable,prerelease aliases=(none) | default=stable | mutability=read-write | ops=get,describe,set,unset | apply=runtime-policy-only | description=Release channel used by all update operations.",
            ]
        );
    }

    #[test]
    fn settings_command_parser_accepts_supported_commands() {
        assert_eq!(SettingsCommand::parse(["list"]), Ok(SettingsCommand::List));
        assert_eq!(
            SettingsCommand::parse(["describe"]),
            Ok(SettingsCommand::Describe(None))
        );
        assert_eq!(
            SettingsCommand::parse(["describe", "screen.backend"]),
            Ok(SettingsCommand::Describe(Some(
                "screen.backend".to_string()
            )))
        );
        assert_eq!(
            SettingsCommand::parse(["get", "screen.backend"]),
            Ok(SettingsCommand::Get("screen.backend".to_string()))
        );
        assert_eq!(
            SettingsCommand::parse(["set", "screen.backend", "gnome"]),
            Ok(SettingsCommand::Set {
                key: "screen.backend".to_string(),
                value: "gnome".to_string(),
            })
        );
        assert_eq!(
            SettingsCommand::parse(["unset", "screen.backend"]),
            Ok(SettingsCommand::Unset("screen.backend".to_string()))
        );
    }

    #[test]
    fn settings_command_parser_rejects_invalid_shapes() {
        assert_eq!(
            SettingsCommand::parse(Vec::<String>::new()),
            Err(SettingsParseError::MissingSubcommand)
        );
        assert_eq!(
            SettingsCommand::parse(["show"]),
            Err(SettingsParseError::UnknownSubcommand("show".to_string()))
        );
        assert_eq!(
            SettingsCommand::parse(["get"]),
            Err(SettingsParseError::MissingKey { subcommand: "get" })
        );
        assert_eq!(
            SettingsCommand::parse(["set", "screen.backend"]),
            Err(SettingsParseError::MissingValue { subcommand: "set" })
        );
        assert_eq!(
            SettingsCommand::parse(["describe", "screen.backend", "extra"]),
            Err(SettingsParseError::UnexpectedArguments {
                subcommand: "describe",
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn settings_runner_lists_values_sources_mutability_and_operations() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner.run(SettingsCommand::List, &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output,
            "\
tv.ip=<missing> (missing, read-write, ops: get,describe,set)
tv.mac=<missing> (missing, read-write, ops: get,describe,set)
tv.input=<missing> (missing, read-write, ops: get,describe,set)
tv.platform=bscpylgtv (default, read-write, ops: get,describe,set,unset)
screen.backend=gnome (config.env, read-write, ops: get,describe,set,unset)
screen.idle_blank=enabled (default, read-write, ops: get,describe,set,unset)
screen.idle_timeout=300 (default, read-write, ops: get,describe,set,unset)
screen.restore_policy=conservative (default, read-write, ops: get,describe,set,unset)
system.sleep_wake_policy=disabled (config.env, read-write, ops: get,describe,set,unset)
updates.auto_check=enabled (default, read-write, ops: get,describe,set,unset)
updates.channel=stable (default, read-write, ops: get,describe,set,unset)
"
        );
    }

    #[test]
    fn settings_runner_get_prints_value_only() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_idle_timeout=450\n").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Get("screen.idle_timeout".to_string()),
                &mut output,
            )
            .unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "450\n");
    }

    #[test]
    fn settings_runner_describe_without_key_describes_all_settings() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(SettingsCommand::Describe(None), &mut output)
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output,
            "\
tv.ip
  storage key: tvs_primary_ip
  type: ipv4
  current: <missing>
  source: missing
  default: required
  mutability: read-write
  supported operations: get, describe, set
  apply: no-runtime-apply-required
  description: IPv4 address of the primary configured TV.

tv.mac
  storage key: tvs_primary_mac
  type: mac-address
  current: <missing>
  source: missing
  default: required
  mutability: read-write
  supported operations: get, describe, set
  apply: no-runtime-apply-required
  description: MAC address of the primary configured TV for Wake-on-LAN.

tv.input
  storage key: tvs_primary_input
  type: enum
  current: <missing>
  source: missing
  default: required
  mutability: read-write
  supported operations: get, describe, set
  allowed values: HDMI_1, HDMI_2, HDMI_3, HDMI_4
  apply: no-runtime-apply-required
  description: HDMI input used by the primary configured TV.

tv.platform
  storage key: tvs_primary_platform
  type: enum
  current: bscpylgtv
  source: default
  default: bscpylgtv
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: bscpylgtv, lg_webos
  apply: no-runtime-apply-required
  description: Control platform for the primary configured TV.

screen.backend
  storage key: screen_backend
  type: enum
  current: auto
  source: default
  default: auto
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: auto, gnome, wayland, swayidle (deprecated compatibility backend)
  apply: restart-user-screen-service
  description: Screen backend selection for user-session blanking and restore behavior.

screen.idle_blank
  storage key: screen_idle_blank
  type: enum
  current: enabled
  source: default
  default: enabled
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: enabled, disabled
  apply: restart-user-screen-service
  description: Idle-driven blanking and restore behavior for the configured screen.

screen.idle_timeout
  storage key: screen_idle_timeout
  type: integer
  current: 300
  source: default
  default: 300
  mutability: read-write
  supported operations: get, describe, set, unset
  range: 1..=86400
  apply: restart-user-screen-service
  description: Idle timeout in seconds before LG Buddy blanks the configured screen.

screen.restore_policy
  storage key: screen_restore_policy
  type: enum
  current: conservative
  source: default
  default: conservative
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: conservative, aggressive
  aliases: marker_only -> conservative
  apply: restart-user-screen-service
  description: Screen restore policy after LG Buddy blanks the configured screen.

system.sleep_wake_policy
  storage key: system_sleep_wake_policy
  type: enum
  current: enabled
  source: default
  default: enabled
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: enabled, disabled
  apply: runtime-policy-only
  description: System sleep and wake policy for lifecycle hooks.

updates.auto_check
  storage key: updates_auto_check
  type: enum
  current: enabled
  source: default
  default: enabled
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: enabled, disabled
  apply: manage-update-check-timer
  description: Automatic background update checks and update notifications.

updates.channel
  storage key: updates_channel
  type: enum
  current: stable
  source: default
  default: stable
  mutability: read-write
  supported operations: get, describe, set, unset
  allowed values: stable, prerelease
  apply: runtime-policy-only
  description: Release channel used by all update operations.
"
        );
    }

    #[test]
    fn settings_runner_rejects_unknown_keys() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Get("screen.unknown".to_string()),
                &mut output,
            )
            .unwrap_err();

        assert_eq!(err, SettingsError::UnknownKey("screen.unknown".to_string()));
        assert!(output.is_empty());
    }

    #[test]
    fn settings_runner_unsets_value_and_removes_all_duplicate_keys() {
        let path = unique_test_path("unset");
        fs::write(
            &path,
            "\
screen_backend=swayidle
screen_idle_timeout=120
screen_idle_timeout=450
screen_restore_policy=aggressive
",
        )
        .unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Unset("screen.idle_timeout".to_string()),
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
screen_backend=swayidle
screen_restore_policy=aggressive
"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: LG_Buddy_screen.service is not installed"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_unsets_absent_key_without_creating_config() {
        let path = unique_test_path("unset-absent");
        let _ = fs::remove_file(&path);
        let store = ConfigEnvReader::empty(&path).into_store();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Unset("screen.backend".to_string()),
                &mut output,
            )
            .unwrap();

        assert!(!path.exists());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.backend already unset"));
        assert!(output.contains("config: unchanged\n"));
        assert!(output.contains("apply: LG_Buddy_screen.service is not installed"));
    }

    #[test]
    fn settings_runner_rejects_invalid_write_without_touching_config() {
        let path = unique_test_path("invalid");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::active_or_enabled()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "kde".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert!(matches!(err, SettingsError::InvalidValue { .. }));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_backend=swayidle\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_rejects_unknown_write_without_touching_config() {
        let path = unique_test_path("unknown");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::active_or_enabled()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.unknown".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(err, SettingsError::UnknownKey("screen.unknown".to_string()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_backend=swayidle\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_creates_parent_directory_for_valid_write() {
        let root = unique_test_path("parent").with_extension("");
        let path = root.join("nested").join("config.env");
        let _ = fs::remove_dir_all(&root);
        let store = ConfigEnvReader::empty(&path).into_store();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::inactive_disabled()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "screen.idle_timeout".to_string(),
                    value: "600".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_idle_timeout=600\n"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: LG_Buddy_screen.service is inactive and disabled"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_path_resolver_reuses_config_path_resolution() {
        let resolved = ConfigPathResolver::resolve(ConfigPathSources {
            explicit_config: Some(Path::new("/tmp/custom.env")),
            install_pointer_config: Some(Path::new("/tmp/pointer.env")),
            sudo_user_home: Some(Path::new("/tmp/sudo-home")),
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok(PathBuf::from("/tmp/custom.env")));
    }

    #[test]
    fn config_env_reader_sanitizes_comments_and_uses_last_duplicate_value() {
        let reader = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=swayidle
            screen_backend=gnome # use GNOME when available
            unused=value
            ",
        );

        assert_eq!(reader.raw_value("screen_backend"), Some("gnome"));
        assert_eq!(reader.raw_value("unused"), Some("value"));
        assert_eq!(reader.raw_value("missing"), None);
    }

    #[test]
    fn settings_store_reads_existing_values_without_required_tv_config() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            screen_idle_blank=disabled
            screen_idle_timeout=450
            screen_restore_policy=aggressive
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();

        let backend = store.effective_by_name("screen.backend").unwrap();
        assert_eq!(backend.value(), Some(SettingValue::Enum("gnome")));
        assert_eq!(backend.source(), SettingSource::ConfigEnv);

        let idle_blank = store.effective_by_name("screen.idle_blank").unwrap();
        assert_eq!(idle_blank.value(), Some(SettingValue::Enum("disabled")));
        assert_eq!(idle_blank.source(), SettingSource::ConfigEnv);

        let idle_timeout = store.effective_by_name("screen.idle_timeout").unwrap();
        assert_eq!(idle_timeout.value(), Some(SettingValue::Integer(450)));
        assert_eq!(idle_timeout.source(), SettingSource::ConfigEnv);

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();
        assert_eq!(
            restore_policy.value(),
            Some(SettingValue::Enum("aggressive"))
        );
        assert_eq!(restore_policy.source(), SettingSource::ConfigEnv);

        let sleep_policy = store.effective_by_name("system.sleep_wake_policy").unwrap();
        assert_eq!(sleep_policy.value(), Some(SettingValue::Enum("disabled")));
        assert_eq!(sleep_policy.source(), SettingSource::ConfigEnv);
    }

    #[test]
    fn settings_store_uses_defaults_for_missing_values() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();

        let effective = store.effective_by_name("screen.idle_timeout").unwrap();

        assert_eq!(effective.value(), Some(SettingValue::Integer(300)));
        assert_eq!(effective.source(), SettingSource::Default);
    }

    #[test]
    fn settings_store_loads_existing_config_file() {
        let path = unique_test_path("existing");
        fs::write(&path, "screen_idle_timeout=123\n").unwrap();

        let store = SettingsStore::load(&path).unwrap();

        assert_eq!(store.path(), path.as_path());
        assert_eq!(store.raw_storage_value("screen_idle_timeout"), Some("123"));
        assert_eq!(
            store
                .effective_by_name("screen.idle_timeout")
                .unwrap()
                .value(),
            Some(SettingValue::Integer(123))
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_store_loads_missing_config_file_as_empty_defaults() {
        let path = unique_test_path("missing");
        let _ = fs::remove_file(&path);

        let store = SettingsStore::load(&path).unwrap();

        assert_eq!(store.path(), path.as_path());
        assert_eq!(
            store.effective_by_name("screen.backend").unwrap().value(),
            Some(SettingValue::Enum("auto"))
        );
        assert_eq!(
            store.effective_by_name("screen.backend").unwrap().source(),
            SettingSource::Default
        );
    }

    #[test]
    fn all_effective_returns_registry_order() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();

        let settings = store.all_effective();
        let keys: Vec<&str> = settings.iter().map(|setting| setting.key_name()).collect();
        let values: Vec<String> = settings.iter().map(format_effective_value).collect();
        let sources: Vec<SettingSource> = settings.iter().map(|setting| setting.source()).collect();

        assert_eq!(
            keys,
            vec![
                "tv.ip",
                "tv.mac",
                "tv.input",
                "tv.platform",
                "screen.backend",
                "screen.idle_blank",
                "screen.idle_timeout",
                "screen.restore_policy",
                "system.sleep_wake_policy",
                "updates.auto_check",
                "updates.channel",
            ]
        );
        assert_eq!(
            values,
            vec![
                "<missing>",
                "<missing>",
                "<missing>",
                "bscpylgtv",
                "gnome",
                "enabled",
                "300",
                "conservative",
                "disabled",
                "enabled",
                "stable",
            ]
        );
        assert_eq!(
            sources,
            vec![
                SettingSource::Missing,
                SettingSource::Missing,
                SettingSource::Missing,
                SettingSource::Default,
                SettingSource::ConfigEnv,
                SettingSource::Default,
                SettingSource::Default,
                SettingSource::Default,
                SettingSource::ConfigEnv,
                SettingSource::Default,
                SettingSource::Default,
            ]
        );
    }

    #[test]
    fn key_parser_accepts_supported_dotted_names() {
        for key in [
            "screen.backend",
            "screen.idle_blank",
            "screen.idle_timeout",
            "screen.restore_policy",
            "system.sleep_wake_policy",
            "updates.auto_check",
            "updates.channel",
        ] {
            assert_eq!(SettingKey::parse(key).unwrap().as_str(), key);
        }
    }

    #[test]
    fn key_parser_rejects_invalid_names() {
        for key in [
            "",
            "screen",
            ".screen.backend",
            "screen.",
            "screen..backend",
            "Screen.backend",
            "screen-backend",
            "screen backend",
        ] {
            assert!(
                matches!(
                    SettingKey::parse(key),
                    Err(SettingsError::InvalidKey { .. })
                ),
                "expected invalid key for `{key}`"
            );
        }
    }

    #[test]
    fn lookup_returns_definitions_by_key() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.key_name(), "screen.idle_timeout");
        assert_eq!(definition.storage_key(), "screen_idle_timeout");
        assert_eq!(definition.default_value(), Some(SettingValue::Integer(300)));
        assert_eq!(definition.mutability(), SettingMutability::ReadWrite);
    }

    #[test]
    fn lookup_rejects_unknown_keys() {
        assert!(matches!(
            SETTINGS_REGISTRY.get_by_name("screen.unknown"),
            Err(SettingsError::UnknownKey(key)) if key == "screen.unknown"
        ));
    }

    #[test]
    fn mutability_controls_supported_operations() {
        let screen_definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();
        assert_eq!(
            screen_definition.supported_operations(),
            &[
                SettingOperation::Get,
                SettingOperation::Describe,
                SettingOperation::Set,
                SettingOperation::Unset,
            ]
        );
        screen_definition
            .ensure_operation_supported(SettingOperation::Set)
            .unwrap();

        let sleep_definition = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert_eq!(sleep_definition.mutability(), SettingMutability::ReadWrite);
        assert_eq!(
            sleep_definition.supported_operations(),
            &[
                SettingOperation::Get,
                SettingOperation::Describe,
                SettingOperation::Set,
                SettingOperation::Unset,
            ]
        );
        sleep_definition
            .ensure_operation_supported(SettingOperation::Set)
            .unwrap();
    }

    #[test]
    fn definitions_expose_type_and_apply_metadata() {
        let idle_timeout = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();
        assert!(matches!(idle_timeout.value_type(), SettingType::Integer(_)));
        assert_eq!(
            idle_timeout.apply_strategy(),
            ApplyStrategy::RestartUserScreenService
        );

        let sleep_policy = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert!(matches!(sleep_policy.value_type(), SettingType::Enum(_)));
        assert_eq!(
            sleep_policy.apply_strategy(),
            ApplyStrategy::RuntimePolicyOnly
        );

        let auto_check = SETTINGS_REGISTRY.get_by_name("updates.auto_check").unwrap();
        assert!(matches!(auto_check.value_type(), SettingType::Enum(_)));
        assert_eq!(
            auto_check.apply_strategy(),
            ApplyStrategy::ManageUpdateCheckTimer
        );

        let update_channel = SETTINGS_REGISTRY.get_by_name("updates.channel").unwrap();
        assert!(matches!(update_channel.value_type(), SettingType::Enum(_)));
        assert_eq!(
            update_channel.apply_strategy(),
            ApplyStrategy::RuntimePolicyOnly
        );
    }

    pub(super) fn unique_test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!(
            "lg-buddy-settings-{name}-{}-{nanos}.env",
            std::process::id()
        ))
    }

    fn setting_definition_contract(definition: &super::SettingDefinition) -> String {
        format!(
            "{} | storage={} | fallbacks={} | type={} | default={} | mutability={} | ops={} | apply={} | description={}",
            definition.key_name(),
            definition.storage_key(),
            format_fallback_storage_keys(definition.fallback_storage_keys()),
            setting_type_contract(definition.value_type()),
            definition.default_value_label(),
            definition.mutability().as_str(),
            format_operation_contract(definition.supported_operations()),
            definition.apply_strategy().as_str(),
            definition.description()
        )
    }

    fn format_fallback_storage_keys(fallbacks: &[&str]) -> String {
        if fallbacks.is_empty() {
            "(none)".to_string()
        } else {
            fallbacks.join(",")
        }
    }

    fn setting_type_contract(value_type: SettingType) -> String {
        match value_type {
            SettingType::Enum(enum_type) => format!(
                "enum values={} aliases={}",
                enum_type.values().join(","),
                format_alias_contract(enum_type.aliases())
            ),
            SettingType::Integer(integer_type) => {
                format!(
                    "integer range={}..={}",
                    integer_type.min(),
                    integer_type.max()
                )
            }
            SettingType::Ipv4 => "ipv4".to_string(),
            SettingType::MacAddress => "mac-address".to_string(),
        }
    }

    fn format_alias_contract(aliases: &[super::SettingAlias]) -> String {
        if aliases.is_empty() {
            "(none)".to_string()
        } else {
            aliases
                .iter()
                .map(|alias| format!("{}->{}", alias.from(), alias.to()))
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    fn format_operation_contract(operations: &[SettingOperation]) -> String {
        operations
            .iter()
            .map(|operation| operation.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    #[derive(Debug, Clone)]
    pub(super) struct FakeServiceController {
        pub(super) state: UserServiceState,
        pub(super) restarts: Rc<Cell<usize>>,
        pub(super) enables: Rc<Cell<usize>>,
        pub(super) disables: Rc<Cell<usize>>,
        pub(super) restart_error: Option<&'static str>,
        pub(super) unit_action_error: Option<&'static str>,
        pub(super) skip_actions: bool,
    }

    impl FakeServiceController {
        pub(super) fn active_or_enabled() -> Self {
            Self {
                state: UserServiceState::ActiveOrEnabled,
                restarts: Rc::new(Cell::new(0)),
                enables: Rc::new(Cell::new(0)),
                disables: Rc::new(Cell::new(0)),
                restart_error: None,
                unit_action_error: None,
                skip_actions: false,
            }
        }

        pub(super) fn inactive_disabled() -> Self {
            Self {
                state: UserServiceState::InactiveDisabled,
                restarts: Rc::new(Cell::new(0)),
                enables: Rc::new(Cell::new(0)),
                disables: Rc::new(Cell::new(0)),
                restart_error: None,
                unit_action_error: None,
                skip_actions: false,
            }
        }

        pub(super) fn missing() -> Self {
            Self {
                state: UserServiceState::Missing,
                restarts: Rc::new(Cell::new(0)),
                enables: Rc::new(Cell::new(0)),
                disables: Rc::new(Cell::new(0)),
                restart_error: None,
                unit_action_error: None,
                skip_actions: false,
            }
        }

        pub(super) fn failing_restart() -> Self {
            Self {
                state: UserServiceState::ActiveOrEnabled,
                restarts: Rc::new(Cell::new(0)),
                enables: Rc::new(Cell::new(0)),
                disables: Rc::new(Cell::new(0)),
                restart_error: Some("restart failed"),
                unit_action_error: None,
                skip_actions: false,
            }
        }

        pub(super) fn failing_unit_action() -> Self {
            Self {
                state: UserServiceState::ActiveOrEnabled,
                restarts: Rc::new(Cell::new(0)),
                enables: Rc::new(Cell::new(0)),
                disables: Rc::new(Cell::new(0)),
                restart_error: None,
                unit_action_error: Some("unit action failed"),
                skip_actions: false,
            }
        }
    }

    impl ServiceController for FakeServiceController {
        fn systemd_actions_disabled(&self) -> bool {
            self.skip_actions
        }

        fn user_service_state(&self, _service: &str) -> Result<UserServiceState, SettingsError> {
            Ok(self.state)
        }

        fn restart_user_service(&self, _service: &str) -> Result<(), SettingsError> {
            self.restarts.set(self.restarts.get() + 1);

            if let Some(message) = self.restart_error {
                Err(SettingsError::Apply {
                    message: message.to_string(),
                })
            } else {
                Ok(())
            }
        }

        fn enable_start_user_unit(
            &self,
            _unit: &str,
        ) -> Result<UserUnitEnableOutcome, SettingsError> {
            self.enables.set(self.enables.get() + 1);

            if let Some(message) = self.unit_action_error {
                Err(SettingsError::Apply {
                    message: message.to_string(),
                })
            } else {
                Ok(UserUnitEnableOutcome::EnabledStarted)
            }
        }

        fn disable_stop_user_unit(&self, _unit: &str) -> Result<(), SettingsError> {
            self.disables.set(self.disables.get() + 1);

            if let Some(message) = self.unit_action_error {
                Err(SettingsError::Apply {
                    message: message.to_string(),
                })
            } else {
                Ok(())
            }
        }
    }
}
