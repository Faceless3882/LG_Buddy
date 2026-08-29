use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

use crate::auth::resolve_config_owner;
use crate::config::TvPlatform;
use crate::platform_access_token::PlatformAccessTokenStore;
use crate::web_os::{
    WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsEndpoint,
    WebOsPowerStateError,
};

use super::{
    ApplyStrategy, EnumSettingType, SettingDefinition, SettingMutability, SettingType,
    SettingValue, SettingsError, SettingsMutation, SettingsMutationAction, SettingsStore,
    EMPTY_ALIASES, EMPTY_STORAGE_KEYS, READ_SET_OPERATIONS, READ_WRITE_OPERATIONS,
};

const IP_FALLBACK_STORAGE_KEYS: &[&str] = &["tv_ip"];
const MAC_FALLBACK_STORAGE_KEYS: &[&str] = &["tv_mac"];
const INPUT_FALLBACK_STORAGE_KEYS: &[&str] = &["input"];
const INPUT_VALUES: &[&str] = &["HDMI_1", "HDMI_2", "HDMI_3", "HDMI_4"];
const PLATFORM_VALUES: &[&str] = &["bscpylgtv", "lg_webos"];

pub(super) const IP: SettingDefinition = SettingDefinition {
    key: "tv.ip",
    storage_key: "tvs_primary_ip",
    fallback_storage_keys: IP_FALLBACK_STORAGE_KEYS,
    value_type: SettingType::Ipv4,
    default_value: None,
    mutability: SettingMutability::ReadWrite,
    operations: READ_SET_OPERATIONS,
    apply_strategy: ApplyStrategy::NoRuntimeApplyRequired,
    description: "IPv4 address of the primary configured TV.",
};

pub(super) const MAC: SettingDefinition = SettingDefinition {
    key: "tv.mac",
    storage_key: "tvs_primary_mac",
    fallback_storage_keys: MAC_FALLBACK_STORAGE_KEYS,
    value_type: SettingType::MacAddress,
    default_value: None,
    mutability: SettingMutability::ReadWrite,
    operations: READ_SET_OPERATIONS,
    apply_strategy: ApplyStrategy::NoRuntimeApplyRequired,
    description: "MAC address of the primary configured TV for Wake-on-LAN.",
};

pub(super) const INPUT: SettingDefinition = SettingDefinition {
    key: "tv.input",
    storage_key: "tvs_primary_input",
    fallback_storage_keys: INPUT_FALLBACK_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: INPUT_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: None,
    mutability: SettingMutability::ReadWrite,
    operations: READ_SET_OPERATIONS,
    apply_strategy: ApplyStrategy::NoRuntimeApplyRequired,
    description: "HDMI input used by the primary configured TV.",
};

pub(super) const PLATFORM: SettingDefinition = SettingDefinition {
    key: "tv.platform",
    storage_key: "tvs_primary_platform",
    fallback_storage_keys: EMPTY_STORAGE_KEYS,
    value_type: SettingType::Enum(EnumSettingType {
        values: PLATFORM_VALUES,
        aliases: EMPTY_ALIASES,
    }),
    default_value: Some(SettingValue::Enum("bscpylgtv")),
    mutability: SettingMutability::ReadWrite,
    operations: READ_WRITE_OPERATIONS,
    apply_strategy: ApplyStrategy::NoRuntimeApplyRequired,
    description: "Control platform for the primary configured TV.",
};

pub trait PlatformPreflight {
    fn preflight(
        &self,
        platform: TvPlatform,
        config_path: &Path,
        tv_ip: Ipv4Addr,
        writer: &mut dyn io::Write,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WebOsPlatformPreflight;

impl PlatformPreflight for WebOsPlatformPreflight {
    fn preflight(
        &self,
        platform: TvPlatform,
        config_path: &Path,
        tv_ip: Ipv4Addr,
        writer: &mut dyn io::Write,
    ) -> Result<(), String> {
        if platform != TvPlatform::LgWebOs {
            return Ok(());
        }

        let owner = resolve_config_owner(config_path).map_err(|error| error.to_string())?;
        let token_store = PlatformAccessTokenStore::for_primary_profile(config_path, owner)
            .map_err(|error| error.to_string())?;

        let mut auth_write_error = None;
        let mut client = {
            let mut on_auth_event = |event| {
                if auth_write_error.is_none() {
                    auth_write_error =
                        write_native_auth_event(writer, token_store.token_path(), event)
                            .err()
                            .map(|error| error.to_string());
                }
            };

            WebOsClient::connect_authenticated(
                WebOsEndpoint::wss(tv_ip),
                Duration::from_secs(3),
                Duration::from_secs(60),
                &token_store,
                &mut on_auth_event,
            )
            .map_err(|error: WebOsAuthenticatedClientError| error.to_string())?
        };

        if let Some(error) = auth_write_error {
            return Err(format!("could not report native pairing progress: {error}"));
        }

        let power_state = client
            .power_state()
            .map_err(|error: WebOsPowerStateError| error.to_string())?;

        writeln!(
            writer,
            "LG Buddy native webOS preflight succeeded: power_state={power_state}"
        )
        .map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())
    }
}

pub(super) fn preflight_if_required<W: io::Write, P: PlatformPreflight>(
    store: &SettingsStore,
    preflight: &P,
    mutation: &SettingsMutation,
    writer: &mut W,
) -> Result<(), SettingsError> {
    if mutation.action() != SettingsMutationAction::Set
        || mutation.key_name() != "tv.platform"
        || mutation.new_value()?.as_enum() != Some(TvPlatform::LgWebOs.as_str())
    {
        return Ok(());
    }

    let tv_ip = match store.effective_by_name("tv.ip")?.required_value()? {
        SettingValue::Ipv4(value) => value,
        _ => {
            return Err(SettingsError::MissingRequiredSetting {
                key: "tv.ip".to_string(),
            });
        }
    };

    preflight
        .preflight(TvPlatform::LgWebOs, store.path(), tv_ip, writer)
        .map_err(|message| SettingsError::PlatformPreflight {
            key: mutation.key_name().to_string(),
            message,
        })
}

fn write_native_auth_event(
    writer: &mut dyn io::Write,
    token_path: &Path,
    event: WebOsAuthenticationEvent,
) -> io::Result<()> {
    match event {
        WebOsAuthenticationEvent::UsingStoredAccessToken => {
            writeln!(
                writer,
                "LG Buddy native webOS preflight: using stored access token."
            )?;
        }
        WebOsAuthenticationEvent::PairingPrompt => {
            writeln!(
                writer,
                "LG Buddy native webOS preflight: pairing required; accept the prompt on the TV."
            )?;
        }
        WebOsAuthenticationEvent::AccessTokenPersisted => {
            writeln!(
                writer,
                "LG Buddy native webOS preflight: stored access token at {}",
                token_path.display()
            )?;
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::super::tests::{unique_test_path, FakeServiceController};
    use super::super::*;
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;
    use std::rc::Rc;

    #[test]
    fn settings_runner_rejects_missing_required_value_on_get() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        let err = runner
            .run(SettingsCommand::Get("tv.ip".to_string()), &mut output)
            .unwrap_err();

        assert_eq!(
            err,
            SettingsError::MissingRequiredSetting {
                key: "tv.ip".to_string()
            }
        );
        assert!(output.is_empty());
    }

    #[test]
    fn settings_runner_rejects_invalid_config_value_on_get() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "tvs_primary_ip=not-an-ip\n").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        let err = runner
            .run(SettingsCommand::Get("tv.ip".to_string()), &mut output)
            .unwrap_err();

        assert_eq!(
            err,
            SettingsError::InvalidValue {
                key: "tv.ip".to_string(),
                value: "not-an-ip".to_string(),
                expected: "an IPv4 address".to_string(),
            }
        );
        assert!(output.is_empty());
    }

    #[test]
    fn settings_runner_lists_invalid_values_from_config() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "tvs_primary_ip=not-an-ip\n").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner.run(SettingsCommand::List, &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("tv.ip=<invalid: not-an-ip> (invalid config.env"));
    }

    #[test]
    fn settings_runner_sets_tv_value_to_canonical_storage_without_restart() {
        let path = unique_test_path("tv-set");
        fs::write(
            &path,
            "\
tv_ip=192.0.2.42
tv_mac=aa:bb:cc:dd:ee:ff
input=HDMI_2
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
                    key: "tv.ip".to_string(),
                    value: "192.0.2.43".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
tv_mac=aa:bb:cc:dd:ee:ff
input=HDMI_2
tvs_primary_ip=192.0.2.43
"
        );
        assert_eq!(restarts.get(), 0);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("tv.ip=192.0.2.43 (saved to "));
        assert!(output.contains("apply: no runtime apply action required\n"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_store_reads_tv_values_from_canonical_and_legacy_storage() {
        let canonical = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            tvs_primary_ip=192.0.2.43
            tvs_primary_mac=11:22:33:44:55:66
            tvs_primary_input=HDMI_3
            ",
        )
        .into_store();

        assert_eq!(
            canonical.effective_by_name("tv.ip").unwrap().value(),
            Some(SettingValue::Ipv4("192.0.2.43".parse().unwrap()))
        );
        assert_eq!(
            canonical.effective_by_name("tv.mac").unwrap().value(),
            Some(SettingValue::MacAddress(
                "11:22:33:44:55:66".parse().unwrap()
            ))
        );
        assert_eq!(
            canonical.effective_by_name("tv.input").unwrap().value(),
            Some(SettingValue::Enum("HDMI_3"))
        );

        let legacy = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            tv_ip=192.0.2.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_2
            ",
        )
        .into_store();

        assert_eq!(
            legacy.effective_by_name("tv.ip").unwrap().value(),
            Some(SettingValue::Ipv4("192.0.2.42".parse().unwrap()))
        );
        assert_eq!(
            legacy.effective_by_name("tv.ip").unwrap().source(),
            SettingSource::LegacyConfigEnv
        );
    }

    #[test]
    fn settings_store_preserves_invalid_required_values() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "tvs_primary_ip=not-an-ip\n").into_store();

        let tv_ip = store.effective_by_name("tv.ip").unwrap();

        assert_eq!(tv_ip.value(), None);
        assert_eq!(tv_ip.source(), SettingSource::InvalidConfigEnv);
        assert_eq!(tv_ip.invalid_value(), Some("not-an-ip"));
        assert_eq!(
            tv_ip.required_value().unwrap_err(),
            SettingsError::InvalidValue {
                key: "tv.ip".to_string(),
                value: "not-an-ip".to_string(),
                expected: "an IPv4 address".to_string(),
            }
        );
    }

    #[test]
    fn tv_values_are_validated() {
        let ip = SETTINGS_REGISTRY.get_by_name("tv.ip").unwrap();
        assert_eq!(
            ip.parse_value("192.0.2.42"),
            Ok(SettingValue::Ipv4("192.0.2.42".parse().unwrap()))
        );
        assert!(matches!(
            ip.parse_value("not-an-ip"),
            Err(SettingsError::InvalidValue { .. })
        ));

        let mac = SETTINGS_REGISTRY.get_by_name("tv.mac").unwrap();
        assert_eq!(
            mac.parse_value("AA:BB:CC:DD:EE:FF"),
            Ok(SettingValue::MacAddress(
                "AA:BB:CC:DD:EE:FF".parse().unwrap()
            ))
        );
        assert!(matches!(
            mac.parse_value("not-a-mac"),
            Err(SettingsError::InvalidValue { .. })
        ));

        let input = SETTINGS_REGISTRY.get_by_name("tv.input").unwrap();
        assert_eq!(
            input.parse_value("HDMI_1"),
            Ok(SettingValue::Enum("HDMI_1"))
        );
        assert!(matches!(
            input.parse_value("AV_1"),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn tv_platform_values_are_validated() {
        let platform = SETTINGS_REGISTRY.get_by_name("tv.platform").unwrap();

        assert_eq!(
            platform.parse_value("bscpylgtv"),
            Ok(SettingValue::Enum("bscpylgtv"))
        );
        assert_eq!(
            platform.parse_value("lg_webos"),
            Ok(SettingValue::Enum("lg_webos"))
        );
        assert!(matches!(
            platform.parse_value("native"),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn settings_runner_requires_successful_native_preflight_before_persisting() {
        let path = unique_test_path("platform-preflight");
        fs::write(&path, "tv_ip=192.0.2.42\ntvs_primary_platform=bscpylgtv\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let preflight = FakePlatformPreflight::succeeding();
        let calls = preflight.calls.clone();
        let runner = SettingsCommandRunner::with_applier_and_preflight(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
            preflight,
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "tv.platform".to_string(),
                    value: "lg_webos".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(calls.get(), 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tv_ip=192.0.2.42\ntvs_primary_platform=lg_webos\n"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("native preflight succeeded"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_native_preflight_leaves_platform_unchanged() {
        let path = unique_test_path("platform-preflight-failure");
        fs::write(&path, "tv_ip=192.0.2.42\ntvs_primary_platform=bscpylgtv\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let preflight = FakePlatformPreflight::failing();
        let calls = preflight.calls.clone();
        let runner = SettingsCommandRunner::with_applier_and_preflight(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
            preflight,
        );
        let mut output = Vec::new();

        let error = runner
            .run(
                SettingsCommand::Set {
                    key: "tv.platform".to_string(),
                    value: "lg_webos".to_string(),
                },
                &mut output,
            )
            .expect_err("failed native preflight should block persistence");

        assert_eq!(calls.get(), 1);
        assert!(matches!(error, SettingsError::PlatformPreflight { .. }));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tv_ip=192.0.2.42\ntvs_primary_platform=bscpylgtv\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn switching_to_bscpylgtv_skips_native_preflight() {
        let path = unique_test_path("platform-legacy");
        fs::write(&path, "tv_ip=192.0.2.42\ntvs_primary_platform=lg_webos\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let preflight = FakePlatformPreflight::failing();
        let calls = preflight.calls.clone();
        let runner = SettingsCommandRunner::with_applier_and_preflight(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
            preflight,
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "tv.platform".to_string(),
                    value: "bscpylgtv".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(calls.get(), 0);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tv_ip=192.0.2.42\ntvs_primary_platform=bscpylgtv\n"
        );

        let _ = fs::remove_file(path);
    }

    #[derive(Debug, Clone)]
    struct FakePlatformPreflight {
        calls: Rc<Cell<usize>>,
        result: Result<(), &'static str>,
    }

    impl FakePlatformPreflight {
        fn succeeding() -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                result: Ok(()),
            }
        }

        fn failing() -> Self {
            Self {
                calls: Rc::new(Cell::new(0)),
                result: Err("TV rejected native authentication"),
            }
        }
    }

    impl PlatformPreflight for FakePlatformPreflight {
        fn preflight(
            &self,
            platform: TvPlatform,
            _config_path: &Path,
            _tv_ip: std::net::Ipv4Addr,
            writer: &mut dyn std::io::Write,
        ) -> Result<(), String> {
            assert_eq!(platform, TvPlatform::LgWebOs);
            self.calls.set(self.calls.get() + 1);
            if self.result.is_ok() {
                writeln!(writer, "native preflight succeeded")
                    .map_err(|error| error.to_string())?;
                Ok(())
            } else {
                self.result.map_err(str::to_string)
            }
        }
    }
}
