use std::collections::HashSet;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use crate::config::{ConfigPathError, MacAddress};

#[derive(Debug, Clone, Copy)]
pub struct SettingsRegistry {
    pub(super) definitions: &'static [SettingDefinition],
}

impl SettingsRegistry {
    pub fn all(&self) -> &'static [SettingDefinition] {
        self.definitions
    }

    pub fn get(&self, key: SettingKey<'_>) -> Result<&'static SettingDefinition, SettingsError> {
        self.definitions
            .iter()
            .find(|definition| definition.key == key.as_str())
            .ok_or_else(|| SettingsError::UnknownKey(key.as_str().to_string()))
    }

    pub fn get_by_name(&self, key: &str) -> Result<&'static SettingDefinition, SettingsError> {
        self.get(SettingKey::parse(key)?)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        let mut public_keys = HashSet::new();
        let mut storage_keys = HashSet::new();

        for definition in self.definitions {
            SettingKey::parse(definition.key)?;
            validate_storage_key(definition.storage_key).map_err(|reason| {
                SettingsError::RegistryInvariant(format!(
                    "invalid storage key `{}` for `{}`: {reason}",
                    definition.storage_key, definition.key
                ))
            })?;

            if !public_keys.insert(definition.key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate setting key `{}`",
                    definition.key
                )));
            }

            if !storage_keys.insert(definition.storage_key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate storage key `{}`",
                    definition.storage_key
                )));
            }

            for fallback_storage_key in definition.fallback_storage_keys {
                validate_storage_key(fallback_storage_key).map_err(|reason| {
                    SettingsError::RegistryInvariant(format!(
                        "invalid fallback storage key `{fallback_storage_key}` for `{}`: {reason}",
                        definition.key
                    ))
                })?;

                if !storage_keys.insert(*fallback_storage_key) {
                    return Err(SettingsError::RegistryInvariant(format!(
                        "duplicate storage key `{fallback_storage_key}`"
                    )));
                }
            }

            definition.validate_type_metadata()?;
            definition.validate_default()?;
            definition.validate_operation_metadata()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettingKey<'a>(&'a str);

impl<'a> SettingKey<'a> {
    pub fn parse(value: &'a str) -> Result<Self, SettingsError> {
        validate_setting_key(value)
            .map_err(|reason| SettingsError::InvalidKey {
                key: value.to_string(),
                reason,
            })
            .map(|()| Self(value))
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl fmt::Display for SettingKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDefinition {
    pub(super) key: &'static str,
    pub(super) storage_key: &'static str,
    pub(super) fallback_storage_keys: &'static [&'static str],
    pub(super) value_type: SettingType,
    pub(super) default_value: Option<SettingValue>,
    pub(super) mutability: SettingMutability,
    pub(super) operations: &'static [SettingOperation],
    pub(super) apply_strategy: ApplyStrategy,
    pub(super) description: &'static str,
}

impl SettingDefinition {
    pub fn key(&self) -> SettingKey<'static> {
        SettingKey(self.key)
    }

    pub fn key_name(&self) -> &'static str {
        self.key
    }

    pub fn storage_key(&self) -> &'static str {
        self.storage_key
    }

    pub fn fallback_storage_keys(&self) -> &'static [&'static str] {
        self.fallback_storage_keys
    }

    pub fn value_type(&self) -> SettingType {
        self.value_type
    }

    pub fn default_value(&self) -> Option<SettingValue> {
        self.default_value
    }

    pub fn default_value_label(&self) -> String {
        self.default_value
            .map(|value| value.to_string())
            .unwrap_or_else(|| "required".to_string())
    }

    pub fn mutability(&self) -> SettingMutability {
        self.mutability
    }

    pub fn supported_operations(&self) -> &'static [SettingOperation] {
        self.operations
    }

    pub fn apply_strategy(&self) -> ApplyStrategy {
        self.apply_strategy
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn supports_operation(&self, operation: SettingOperation) -> bool {
        self.operations.contains(&operation)
    }

    pub fn ensure_operation_supported(
        &self,
        operation: SettingOperation,
    ) -> Result<(), SettingsError> {
        if self.supports_operation(operation) {
            Ok(())
        } else {
            Err(SettingsError::UnsupportedOperation {
                key: self.key.to_string(),
                operation,
            })
        }
    }

    pub fn parse_value(&self, value: &str) -> Result<SettingValue, SettingsError> {
        self.value_type.parse_value(self.key, value)
    }

    fn validate_type_metadata(&self) -> Result<(), SettingsError> {
        match self.value_type {
            SettingType::Enum(enum_type) => enum_type.validate(self.key),
            SettingType::Integer(integer_type) => integer_type.validate(self.key),
            SettingType::Ipv4 | SettingType::MacAddress => Ok(()),
        }
    }

    fn validate_default(&self) -> Result<(), SettingsError> {
        if let Some(default_value) = self.default_value {
            self.value_type
                .validate_value(self.key, default_value)
                .map_err(|err| match err {
                    SettingsError::InvalidValue {
                        key,
                        value,
                        expected,
                    } => SettingsError::RegistryInvariant(format!(
                        "invalid default value `{value}` for `{key}`: expected {expected}"
                    )),
                    other => other,
                })
        } else {
            Ok(())
        }
    }

    fn validate_operation_metadata(&self) -> Result<(), SettingsError> {
        if self.operations.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{}` must support at least one operation",
                self.key
            )));
        }

        match self.mutability {
            SettingMutability::ReadWrite => {
                for operation in [
                    SettingOperation::Get,
                    SettingOperation::Describe,
                    SettingOperation::Set,
                ] {
                    if !self.operations.contains(&operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-write but does not support `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
            SettingMutability::ReadOnly => {
                for operation in [SettingOperation::Set, SettingOperation::Unset] {
                    if self.operations.contains(&operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-only but supports `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
        }

        if self.default_value.is_none() && self.operations.contains(&SettingOperation::Unset) {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{}` is required but supports `unset`",
                self.key
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingType {
    Enum(EnumSettingType),
    Integer(IntegerSettingType),
    Ipv4,
    MacAddress,
}

impl SettingType {
    pub fn parse_value(self, key: &str, value: &str) -> Result<SettingValue, SettingsError> {
        match self {
            Self::Enum(enum_type) => enum_type
                .canonicalize(value)
                .map(SettingValue::Enum)
                .ok_or_else(|| SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: enum_type.expected(),
                }),
            Self::Integer(integer_type) => match value.parse::<i128>() {
                Ok(parsed) if integer_type.accepts(parsed) => {
                    Ok(SettingValue::Integer(integer_type.coerce(parsed)))
                }
                _ => Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                }),
            },
            Self::Ipv4 => value
                .parse::<Ipv4Addr>()
                .map(SettingValue::Ipv4)
                .map_err(|_| SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: Self::Ipv4.expected(),
                }),
            Self::MacAddress => value
                .parse::<MacAddress>()
                .map(SettingValue::MacAddress)
                .map_err(|_| SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: Self::MacAddress.expected(),
                }),
        }
    }

    pub fn validate_value(self, key: &str, value: SettingValue) -> Result<(), SettingsError> {
        match (self, value) {
            (Self::Enum(enum_type), SettingValue::Enum(value)) => {
                match enum_type.canonicalize(value) {
                    Some(canonical) if canonical == value => Ok(()),
                    _ => Err(SettingsError::InvalidValue {
                        key: key.to_string(),
                        value: value.to_string(),
                        expected: enum_type.expected(),
                    }),
                }
            }
            (Self::Integer(integer_type), SettingValue::Integer(value))
                if integer_type.contains(value) =>
            {
                Ok(())
            }
            (Self::Integer(integer_type), SettingValue::Integer(value)) => {
                Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                })
            }
            (Self::Ipv4, SettingValue::Ipv4(_))
            | (Self::MacAddress, SettingValue::MacAddress(_)) => Ok(()),
            (expected_type, actual_value) => Err(SettingsError::InvalidValue {
                key: key.to_string(),
                value: actual_value.to_string(),
                expected: expected_type.expected(),
            }),
        }
    }

    pub fn expected(self) -> String {
        match self {
            Self::Enum(enum_type) => enum_type.expected(),
            Self::Integer(integer_type) => integer_type.expected(),
            Self::Ipv4 => "an IPv4 address".to_string(),
            Self::MacAddress => "a MAC address like aa:bb:cc:dd:ee:ff".to_string(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enum(_) => "enum",
            Self::Integer(_) => "integer",
            Self::Ipv4 => "ipv4",
            Self::MacAddress => "mac-address",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumSettingType {
    pub(super) values: &'static [&'static str],
    pub(super) aliases: &'static [SettingAlias],
}

impl EnumSettingType {
    pub fn values(self) -> &'static [&'static str] {
        self.values
    }

    pub fn aliases(self) -> &'static [SettingAlias] {
        self.aliases
    }

    pub fn canonicalize(self, value: &str) -> Option<&'static str> {
        if let Some(canonical) = self
            .values
            .iter()
            .copied()
            .find(|allowed| *allowed == value)
        {
            return Some(canonical);
        }

        self.aliases
            .iter()
            .find(|alias| alias.from == value)
            .map(|alias| alias.to)
    }

    fn expected(self) -> String {
        format!("one of {}", self.values.join(", "))
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.values.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{key}` enum settings must define at least one value"
            )));
        }

        let mut values = HashSet::new();
        for value in self.values {
            if value.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty values"
                )));
            }

            if !values.insert(*value) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate value `{value}`"
                )));
            }
        }

        let mut aliases = HashSet::new();
        for alias in self.aliases {
            if alias.from.is_empty() || alias.to.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty aliases"
                )));
            }

            if values.contains(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` duplicates a canonical value",
                    alias.from
                )));
            }

            if !values.contains(alias.to) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` points to unknown value `{}`",
                    alias.from, alias.to
                )));
            }

            if !aliases.insert(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate alias `{}`",
                    alias.from
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingAlias {
    pub(super) from: &'static str,
    pub(super) to: &'static str,
}

impl SettingAlias {
    pub fn from(self) -> &'static str {
        self.from
    }

    pub fn to(self) -> &'static str {
        self.to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerSettingType {
    pub(super) min: i64,
    pub(super) max: i64,
}

impl IntegerSettingType {
    pub fn min(self) -> i64 {
        self.min
    }

    pub fn max(self) -> i64 {
        self.max
    }

    pub fn contains(self, value: i64) -> bool {
        value >= self.min && value <= self.max
    }

    fn accepts(self, value: i128) -> bool {
        value >= i128::from(self.min)
    }

    fn coerce(self, value: i128) -> i64 {
        if value > i128::from(self.max) {
            self.max
        } else {
            value as i64
        }
    }

    fn expected(self) -> String {
        format!("an integer from {} to {}", self.min, self.max)
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.min <= self.max {
            Ok(())
        } else {
            Err(SettingsError::RegistryInvariant(format!(
                "`{key}` integer setting has an invalid range {}..{}",
                self.min, self.max
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValue {
    Enum(&'static str),
    Integer(i64),
    Ipv4(Ipv4Addr),
    MacAddress(MacAddress),
}

impl SettingValue {
    pub fn as_enum(self) -> Option<&'static str> {
        match self {
            Self::Enum(value) => Some(value),
            Self::Integer(_) | Self::Ipv4(_) | Self::MacAddress(_) => None,
        }
    }

    pub fn as_integer(self) -> Option<i64> {
        match self {
            Self::Enum(_) | Self::Ipv4(_) | Self::MacAddress(_) => None,
            Self::Integer(value) => Some(value),
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enum(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Ipv4(value) => write!(f, "{value}"),
            Self::MacAddress(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingMutability {
    ReadOnly,
    ReadWrite,
}

impl SettingMutability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingOperation {
    Get,
    Describe,
    Set,
    Unset,
}

impl SettingOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Describe => "describe",
            Self::Set => "set",
            Self::Unset => "unset",
        }
    }
}

impl fmt::Display for SettingOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    RestartUserScreenService,
    ManageUpdateCheckTimer,
    RuntimePolicyOnly,
    NoRuntimeApplyRequired,
}

impl ApplyStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RestartUserScreenService => "restart-user-screen-service",
            Self::ManageUpdateCheckTimer => "manage-update-check-timer",
            Self::RuntimePolicyOnly => "runtime-policy-only",
            Self::NoRuntimeApplyRequired => "no-runtime-apply-required",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    ConfigPath(ConfigPathError),
    ReadConfig {
        path: PathBuf,
        kind: io::ErrorKind,
        message: String,
    },
    WriteConfig {
        path: PathBuf,
        message: String,
    },
    Apply {
        message: String,
    },
    PlatformPreflight {
        key: String,
        message: String,
    },
    ApplyAfterPersist {
        key: String,
        path: PathBuf,
        message: String,
    },
    WriteOutput(String),
    InvalidKey {
        key: String,
        reason: &'static str,
    },
    UnknownKey(String),
    InvalidValue {
        key: String,
        value: String,
        expected: String,
    },
    MissingRequiredSetting {
        key: String,
    },
    UnsupportedOperation {
        key: String,
        operation: SettingOperation,
    },
    RegistryInvariant(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::ReadConfig { path, message, .. } => {
                write!(
                    f,
                    "could not read settings config `{}`: {message}",
                    path.display()
                )
            }
            Self::WriteConfig { path, message } => {
                write!(
                    f,
                    "could not write settings config `{}`: {message}",
                    path.display()
                )
            }
            Self::Apply { message } => write!(f, "{message}"),
            Self::PlatformPreflight { key, message } => write!(
                f,
                "could not enable platform setting `{key}` because native preflight failed: {message}"
            ),
            Self::ApplyAfterPersist { key, path, message } => write!(
                f,
                "setting `{key}` was saved to `{}` but could not be applied: {message}. Restart LG Buddy or rerun the command after fixing the apply error.",
                path.display()
            ),
            Self::WriteOutput(message) => write!(f, "{message}"),
            Self::InvalidKey { key, reason } => {
                write!(f, "invalid setting key `{key}`: {reason}")
            }
            Self::UnknownKey(key) => write!(f, "unknown setting `{key}`"),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                f,
                "invalid value for setting `{key}`: `{value}`; expected {expected}"
            ),
            Self::MissingRequiredSetting { key } => {
                write!(f, "required setting `{key}` is not configured")
            }
            Self::UnsupportedOperation { key, operation } => {
                write!(
                    f,
                    "setting `{key}` does not support `{}`",
                    operation.as_str()
                )
            }
            Self::RegistryInvariant(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigPath(err) => Some(err),
            Self::ReadConfig { .. }
            | Self::WriteConfig { .. }
            | Self::Apply { .. }
            | Self::PlatformPreflight { .. }
            | Self::ApplyAfterPersist { .. }
            | Self::WriteOutput(_)
            | Self::InvalidKey { .. }
            | Self::UnknownKey(_)
            | Self::InvalidValue { .. }
            | Self::MissingRequiredSetting { .. }
            | Self::UnsupportedOperation { .. }
            | Self::RegistryInvariant(_) => None,
        }
    }
}

fn validate_setting_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    let mut last_was_dot = true;
    let mut has_dot = false;

    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'_' => {
                last_was_dot = false;
            }
            b'.' if !last_was_dot => {
                last_was_dot = true;
                has_dot = true;
            }
            b'.' => return Err("must not contain empty segments"),
            _ => {
                return Err(
                    "must contain only ASCII lowercase letters, digits, underscores, and dots",
                )
            }
        }
    }

    if last_was_dot {
        return Err("must not end with a dot");
    }

    if !has_dot {
        return Err("must contain at least one dot");
    }

    Ok(())
}

fn validate_storage_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    if value
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        Ok(())
    } else {
        Err("must contain only ASCII lowercase letters, digits, and underscores")
    }
}
