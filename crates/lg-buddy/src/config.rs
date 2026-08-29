use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::{collections::HashMap, io};

const CONFIG_DIR_NAME: &str = "lg-buddy";
const CONFIG_FILE_NAME: &str = "config.env";
const INSTALL_CONFIG_POINTER_FILE: &str = "/usr/lib/lg-buddy/config-path";
pub const DEFAULT_IDLE_TIMEOUT: u64 = 300;
pub const MAX_IDLE_TIMEOUT: u64 = 86_400;

pub fn parse_idle_timeout_secs(value: &str) -> Option<u64> {
    value.parse::<u128>().ok().map(normalize_idle_timeout_secs)
}

pub fn normalize_idle_timeout_secs(idle_timeout_secs: u128) -> u64 {
    if idle_timeout_secs == 0 {
        DEFAULT_IDLE_TIMEOUT
    } else if idle_timeout_secs > u128::from(MAX_IDLE_TIMEOUT) {
        MAX_IDLE_TIMEOUT
    } else {
        idle_timeout_secs as u64
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigPathSources<'a> {
    pub explicit_config: Option<&'a Path>,
    pub install_pointer_config: Option<&'a Path>,
    pub sudo_user_home: Option<&'a Path>,
    pub xdg_config_home: Option<&'a Path>,
    pub home: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPathError {
    NotConfigured,
}

impl fmt::Display for ConfigPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => {
                write!(
                    f,
                    "could not resolve a config path from override, XDG, or HOME"
                )
            }
        }
    }
}

impl Error for ConfigPathError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenBackend {
    Auto,
    Gnome,
    Wayland,
    Swayidle,
}

impl ScreenBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gnome => "gnome",
            Self::Wayland => "wayland",
            Self::Swayidle => "swayidle",
        }
    }
}

impl FromStr for ScreenBackend {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "gnome" => Ok(Self::Gnome),
            "wayland" => Ok(Self::Wayland),
            "swayidle" => Ok(Self::Swayidle),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenRestorePolicy {
    MarkerOnly,
    Aggressive,
}

impl ScreenRestorePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MarkerOnly => "marker_only",
            Self::Aggressive => "aggressive",
        }
    }
}

impl FromStr for ScreenRestorePolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "marker_only" | "conservative" => Ok(Self::MarkerOnly),
            "aggressive" => Ok(Self::Aggressive),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenIdleBlankPolicy {
    Enabled,
    Disabled,
}

impl ScreenIdleBlankPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl FromStr for ScreenIdleBlankPolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "enabled" => Ok(Self::Enabled),
            "disabled" => Ok(Self::Disabled),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSleepWakePolicy {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvPlatform {
    Bscpylgtv,
    LgWebOs,
}

impl TvPlatform {
    pub const DEFAULT: Self = Self::Bscpylgtv;

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bscpylgtv => "bscpylgtv",
            Self::LgWebOs => "lg_webos",
        }
    }
}

impl fmt::Display for TvPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TvPlatform {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "bscpylgtv" => Ok(Self::Bscpylgtv),
            "lg_webos" => Ok(Self::LgWebOs),
            _ => Err(()),
        }
    }
}

impl SystemSleepWakePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl FromStr for SystemSleepWakePolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "enabled" => Ok(Self::Enabled),
            "disabled" => Ok(Self::Disabled),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdmiInput {
    Hdmi1,
    Hdmi2,
    Hdmi3,
    Hdmi4,
}

impl HdmiInput {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hdmi1 => "HDMI_1",
            Self::Hdmi2 => "HDMI_2",
            Self::Hdmi3 => "HDMI_3",
            Self::Hdmi4 => "HDMI_4",
        }
    }

    pub fn expected_app_id(&self) -> &'static str {
        match self {
            Self::Hdmi1 => "com.webos.app.hdmi1",
            Self::Hdmi2 => "com.webos.app.hdmi2",
            Self::Hdmi3 => "com.webos.app.hdmi3",
            Self::Hdmi4 => "com.webos.app.hdmi4",
        }
    }

    pub fn from_app_id(value: &str) -> Option<Self> {
        match value {
            "com.webos.app.hdmi1" => Some(Self::Hdmi1),
            "com.webos.app.hdmi2" => Some(Self::Hdmi2),
            "com.webos.app.hdmi3" => Some(Self::Hdmi3),
            "com.webos.app.hdmi4" => Some(Self::Hdmi4),
            _ => None,
        }
    }
}

impl FromStr for HdmiInput {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "HDMI_1" => Ok(Self::Hdmi1),
            "HDMI_2" => Ok(Self::Hdmi2),
            "HDMI_3" => Ok(Self::Hdmi3),
            "HDMI_4" => Ok(Self::Hdmi4),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    pub fn octets(&self) -> [u8; 6] {
        self.0
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl FromStr for MacAddress {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut octets = [0_u8; 6];
        let parts: Vec<&str> = value.split(':').collect();
        if parts.len() != 6 {
            return Err(());
        }

        for (index, part) in parts.iter().enumerate() {
            if part.len() != 2 {
                return Err(());
            }

            octets[index] = u8::from_str_radix(part, 16).map_err(|_| ())?;
        }

        Ok(Self(octets))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub tv_ip: Ipv4Addr,
    pub tv_mac: MacAddress,
    pub input: HdmiInput,
    pub tv_platform: TvPlatform,
    pub screen_backend: ScreenBackend,
    pub screen_idle_timeout: u64,
    pub screen_restore_policy: ScreenRestorePolicy,
    pub screen_idle_blank: ScreenIdleBlankPolicy,
    pub system_sleep_wake_policy: SystemSleepWakePolicy,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    MissingRequiredKey(&'static str),
    InvalidValue {
        key: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::MissingRequiredKey(key) => write!(f, "missing required config key `{key}`"),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                f,
                "invalid value for `{key}`: `{value}`; expected {expected}"
            ),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::MissingRequiredKey(_) | Self::InvalidValue { .. } => None,
        }
    }
}

impl From<io::Error> for ConfigError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn resolve_config_path(sources: ConfigPathSources<'_>) -> Result<PathBuf, ConfigPathError> {
    if let Some(path) = sources.explicit_config {
        return Ok(path.to_path_buf());
    }

    if let Some(path) = sources.install_pointer_config {
        return Ok(path.to_path_buf());
    }

    if let Some(path) = sources.sudo_user_home {
        return Ok(path
            .join(".config")
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME));
    }

    if let Some(path) = sources.xdg_config_home {
        return Ok(path.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME));
    }

    if let Some(path) = sources.home {
        return Ok(path
            .join(".config")
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME));
    }

    Err(ConfigPathError::NotConfigured)
}

pub fn resolve_config_path_from_env() -> Result<PathBuf, ConfigPathError> {
    let explicit_config = env::var_os("LG_BUDDY_CONFIG").map(PathBuf::from);
    let install_pointer_config = load_install_pointer_config();
    let sudo_user_home = resolve_sudo_user_home();
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);

    resolve_config_path(ConfigPathSources {
        explicit_config: explicit_config.as_deref(),
        install_pointer_config: install_pointer_config.as_deref(),
        sudo_user_home: sudo_user_home.as_deref(),
        xdg_config_home: xdg_config_home.as_deref(),
        home: home.as_deref(),
    })
}

fn load_install_pointer_config() -> Option<PathBuf> {
    let contents = fs::read_to_string(INSTALL_CONFIG_POINTER_FILE).ok()?;
    let path = contents.lines().next()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn resolve_sudo_user_home() -> Option<PathBuf> {
    let sudo_user = env::var_os("SUDO_USER")?;
    let sudo_user = sudo_user.to_string_lossy();
    if sudo_user.is_empty() || sudo_user == "root" {
        return None;
    }

    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    parse_home_from_passwd_entries(&passwd, &sudo_user)
}

fn parse_home_from_passwd_entries(contents: &str, user: &str) -> Option<PathBuf> {
    for line in contents.lines() {
        let mut fields = line.split(':');
        let username = fields.next()?;
        if username != user {
            continue;
        }

        let home = fields.nth(4)?;
        if home.is_empty() {
            return None;
        }

        return Some(PathBuf::from(home));
    }

    None
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let contents = fs::read_to_string(path)?;
    parse_config(&contents)
}

pub fn parse_config(contents: &str) -> Result<Config, ConfigError> {
    let entries = parse_config_entries(contents);

    let tv_ip = required_config_value(&entries, "tvs_primary_ip", &["tv_ip"])
        .ok_or(ConfigError::MissingRequiredKey("tvs_primary_ip"))
        .and_then(|value| {
            value
                .parse::<Ipv4Addr>()
                .map_err(|_| ConfigError::InvalidValue {
                    key: "tvs_primary_ip",
                    value: value.clone(),
                    expected: "an IPv4 address",
                })
        })?;

    let tv_mac = required_config_value(&entries, "tvs_primary_mac", &["tv_mac"])
        .ok_or(ConfigError::MissingRequiredKey("tvs_primary_mac"))
        .and_then(|value| {
            value
                .parse::<MacAddress>()
                .map_err(|_| ConfigError::InvalidValue {
                    key: "tvs_primary_mac",
                    value: value.clone(),
                    expected: "a MAC address like aa:bb:cc:dd:ee:ff",
                })
        })?;

    let input = required_config_value(&entries, "tvs_primary_input", &["input"])
        .ok_or(ConfigError::MissingRequiredKey("tvs_primary_input"))
        .and_then(|value| {
            value
                .parse::<HdmiInput>()
                .map_err(|_| ConfigError::InvalidValue {
                    key: "tvs_primary_input",
                    value: value.clone(),
                    expected: "one of HDMI_1, HDMI_2, HDMI_3, HDMI_4",
                })
        })?;

    let tv_platform = match entries.get("tvs_primary_platform") {
        None => TvPlatform::DEFAULT,
        Some(value) => value
            .parse::<TvPlatform>()
            .map_err(|_| ConfigError::InvalidValue {
                key: "tvs_primary_platform",
                value: value.clone(),
                expected: "one of bscpylgtv, lg_webos",
            })?,
    };

    let screen_backend = entries
        .get("screen_backend")
        .and_then(|value| value.parse::<ScreenBackend>().ok())
        .unwrap_or(ScreenBackend::Auto);

    let screen_idle_timeout = entries
        .get("screen_idle_timeout")
        .and_then(|value| parse_idle_timeout_secs(value))
        .unwrap_or(DEFAULT_IDLE_TIMEOUT);

    let screen_restore_policy = entries
        .get("screen_restore_policy")
        .and_then(|value| value.parse::<ScreenRestorePolicy>().ok())
        .unwrap_or(ScreenRestorePolicy::MarkerOnly);

    let screen_idle_blank = entries
        .get("screen_idle_blank")
        .and_then(|value| value.parse::<ScreenIdleBlankPolicy>().ok())
        .unwrap_or(ScreenIdleBlankPolicy::Enabled);

    let system_sleep_wake_policy = entries
        .get("system_sleep_wake_policy")
        .and_then(|value| value.parse::<SystemSleepWakePolicy>().ok())
        .unwrap_or(SystemSleepWakePolicy::Enabled);

    Ok(Config {
        tv_ip,
        tv_mac,
        input,
        tv_platform,
        screen_backend,
        screen_idle_timeout,
        screen_restore_policy,
        screen_idle_blank,
        system_sleep_wake_policy,
    })
}

fn required_config_value<'a>(
    entries: &'a HashMap<String, String>,
    primary_key: &str,
    fallback_keys: &[&str],
) -> Option<&'a String> {
    entries.get(primary_key).or_else(|| {
        fallback_keys
            .iter()
            .find_map(|fallback_key| entries.get(*fallback_key))
    })
}

pub(crate) fn parse_config_entries(contents: &str) -> HashMap<String, String> {
    let mut entries = HashMap::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        entries.insert(key.trim().to_string(), sanitize_config_value(value));
    }

    entries
}

fn sanitize_config_value(value: &str) -> String {
    value
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_config, parse_home_from_passwd_entries, resolve_config_path, Config, ConfigError,
        ConfigPathError, ConfigPathSources, HdmiInput, ScreenBackend, ScreenIdleBlankPolicy,
        ScreenRestorePolicy, SystemSleepWakePolicy, TvPlatform, DEFAULT_IDLE_TIMEOUT,
        MAX_IDLE_TIMEOUT,
    };
    use std::path::Path;

    #[test]
    fn explicit_config_override_wins() {
        let resolved = resolve_config_path(ConfigPathSources {
            explicit_config: Some(Path::new("/tmp/custom.env")),
            install_pointer_config: Some(Path::new("/tmp/pointer.env")),
            sudo_user_home: Some(Path::new("/tmp/sudo-home")),
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok("/tmp/custom.env".into()));
    }

    #[test]
    fn xdg_config_home_is_used_when_override_is_missing() {
        let resolved = resolve_config_path(ConfigPathSources {
            explicit_config: None,
            install_pointer_config: None,
            sudo_user_home: None,
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok("/tmp/xdg/lg-buddy/config.env".into()));
    }

    #[test]
    fn home_is_used_when_xdg_is_missing() {
        let resolved = resolve_config_path(ConfigPathSources {
            explicit_config: None,
            install_pointer_config: None,
            sudo_user_home: None,
            xdg_config_home: None,
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok("/tmp/home/.config/lg-buddy/config.env".into()));
    }

    #[test]
    fn missing_inputs_returns_error() {
        let resolved = resolve_config_path(ConfigPathSources::default());

        assert_eq!(resolved, Err(ConfigPathError::NotConfigured));
    }

    #[test]
    fn install_pointer_config_is_used_before_user_env_paths() {
        let resolved = resolve_config_path(ConfigPathSources {
            explicit_config: None,
            install_pointer_config: Some(Path::new("/tmp/pointer.env")),
            sudo_user_home: Some(Path::new("/tmp/sudo-home")),
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok("/tmp/pointer.env".into()));
    }

    #[test]
    fn sudo_user_home_is_used_before_current_home() {
        let resolved = resolve_config_path(ConfigPathSources {
            explicit_config: None,
            install_pointer_config: None,
            sudo_user_home: Some(Path::new("/home/installing-user")),
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/root")),
        });

        assert_eq!(
            resolved,
            Ok("/home/installing-user/.config/lg-buddy/config.env".into())
        );
    }

    #[test]
    fn passwd_home_lookup_returns_matching_user_home() {
        let passwd = "\
root:x:0:0:root:/root:/bin/bash\n\
vas:x:1000:1000:vas:/home/vas:/bin/bash\n";

        let home = parse_home_from_passwd_entries(passwd, "vas");

        assert_eq!(home, Some("/home/vas".into()));
    }

    #[test]
    fn parse_valid_config() {
        let config = parse_config(
            "\
            # LG Buddy configuration
            tvs_primary_ip=192.168.1.42
            tvs_primary_mac=aa:bb:cc:dd:ee:ff
            tvs_primary_input=HDMI_2
            tvs_primary_platform=lg_webos
            screen_backend=wayland
            screen_idle_timeout=450
            screen_restore_policy=aggressive
            screen_idle_blank=disabled
            system_sleep_wake_policy=disabled
            ",
        )
        .expect("parse valid config");

        assert_eq!(config.tv_ip.to_string(), "192.168.1.42");
        assert_eq!(config.tv_mac.to_string(), "aa:bb:cc:dd:ee:ff");
        assert_eq!(config.input, HdmiInput::Hdmi2);
        assert_eq!(config.tv_platform, TvPlatform::LgWebOs);
        assert_eq!(config.screen_backend, ScreenBackend::Wayland);
        assert_eq!(config.screen_idle_timeout, 450);
        assert_eq!(
            config.screen_restore_policy,
            ScreenRestorePolicy::Aggressive
        );
        assert_eq!(config.screen_idle_blank, ScreenIdleBlankPolicy::Disabled);
        assert_eq!(
            config.system_sleep_wake_policy,
            SystemSleepWakePolicy::Disabled
        );
    }

    #[test]
    fn parse_clamps_idle_timeout_to_max() {
        let config = parse_config(
            "\
            tvs_primary_ip=192.168.1.42
            tvs_primary_mac=aa:bb:cc:dd:ee:ff
            tvs_primary_input=HDMI_2
            screen_idle_timeout=86401
            ",
        )
        .expect("parse config with capped timeout");

        assert_eq!(config.screen_idle_timeout, MAX_IDLE_TIMEOUT);

        let config = parse_config(
            "\
            tvs_primary_ip=192.168.1.42
            tvs_primary_mac=aa:bb:cc:dd:ee:ff
            tvs_primary_input=HDMI_2
            screen_idle_timeout=18446744073709551615
            ",
        )
        .expect("parse config with u64-max timeout");

        assert_eq!(config.screen_idle_timeout, MAX_IDLE_TIMEOUT);
    }

    #[test]
    fn parse_accepts_legacy_single_tv_keys() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_2
            ",
        )
        .expect("parse legacy TV config");

        assert_eq!(config.tv_ip.to_string(), "192.168.1.42");
        assert_eq!(config.tv_mac.to_string(), "aa:bb:cc:dd:ee:ff");
        assert_eq!(config.input, HdmiInput::Hdmi2);
    }

    #[test]
    fn hdmi_input_round_trips_through_app_id() {
        for input in [
            HdmiInput::Hdmi1,
            HdmiInput::Hdmi2,
            HdmiInput::Hdmi3,
            HdmiInput::Hdmi4,
        ] {
            assert_eq!(HdmiInput::from_app_id(input.expected_app_id()), Some(input));
        }
    }

    #[test]
    fn parse_uses_defaults_for_optional_values() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            ",
        )
        .expect("parse config with defaults");

        assert_eq!(
            config,
            Config {
                tv_ip: "192.168.1.42".parse().expect("ipv4"),
                tv_mac: "aa:bb:cc:dd:ee:ff".parse().expect("mac"),
                input: HdmiInput::Hdmi1,
                tv_platform: TvPlatform::Bscpylgtv,
                screen_backend: ScreenBackend::Auto,
                screen_idle_timeout: DEFAULT_IDLE_TIMEOUT,
                screen_restore_policy: ScreenRestorePolicy::MarkerOnly,
                screen_idle_blank: ScreenIdleBlankPolicy::Enabled,
                system_sleep_wake_policy: SystemSleepWakePolicy::Enabled,
            }
        );
    }

    #[test]
    fn parse_sanitizes_inline_comments_on_values() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42 # living room
            tv_mac=aa:bb:cc:dd:ee:ff # detected
            input=HDMI_3 # main PC
            screen_backend=gnome # use GNOME
            screen_idle_timeout=450 # seconds
            screen_restore_policy=aggressive # restore on wake without a marker
            screen_idle_blank=disabled # keep the session agent passive
            system_sleep_wake_policy=disabled # disable lifecycle TV control
            ",
        )
        .expect("parse sanitized config");

        assert_eq!(config.tv_ip.to_string(), "192.168.1.42");
        assert_eq!(config.tv_mac.to_string(), "aa:bb:cc:dd:ee:ff");
        assert_eq!(config.input, HdmiInput::Hdmi3);
        assert_eq!(config.screen_backend, ScreenBackend::Gnome);
        assert_eq!(config.screen_idle_timeout, 450);
        assert_eq!(
            config.screen_restore_policy,
            ScreenRestorePolicy::Aggressive
        );
        assert_eq!(config.screen_idle_blank, ScreenIdleBlankPolicy::Disabled);
        assert_eq!(
            config.system_sleep_wake_policy,
            SystemSleepWakePolicy::Disabled
        );
    }

    #[test]
    fn parse_accepts_conservative_restore_policy_alias() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            screen_restore_policy=conservative
            ",
        )
        .expect("parse config with conservative alias");

        assert_eq!(
            config.screen_restore_policy,
            ScreenRestorePolicy::MarkerOnly
        );
    }

    #[test]
    fn parse_invalid_optional_values_fall_back_to_defaults() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            screen_backend=not-a-backend
            screen_idle_timeout=not-a-number
            screen_restore_policy=not-a-policy
            screen_idle_blank=not-a-policy
            system_sleep_wake_policy=not-a-policy
            ",
        )
        .expect("parse config with malformed optional values");

        assert_eq!(config.screen_backend, ScreenBackend::Auto);
        assert_eq!(config.screen_idle_timeout, DEFAULT_IDLE_TIMEOUT);
        assert_eq!(
            config.screen_restore_policy,
            ScreenRestorePolicy::MarkerOnly
        );
        assert_eq!(config.screen_idle_blank, ScreenIdleBlankPolicy::Enabled);
        assert_eq!(
            config.system_sleep_wake_policy,
            SystemSleepWakePolicy::Enabled
        );
    }

    #[test]
    fn parse_defaults_missing_tv_platform_to_bscpylgtv() {
        let config = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            ",
        )
        .expect("parse config with compatibility platform default");

        assert_eq!(config.tv_platform, TvPlatform::Bscpylgtv);
    }

    #[test]
    fn parse_rejects_invalid_explicit_tv_platform() {
        let err = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            tvs_primary_platform=not-a-platform
            ",
        )
        .expect_err("invalid explicit platform should fail");

        assert_eq!(
            err.to_string(),
            "invalid value for `tvs_primary_platform`: `not-a-platform`; expected one of bscpylgtv, lg_webos"
        );
    }

    #[test]
    fn parse_accepts_system_sleep_wake_policy_values() {
        let enabled = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            system_sleep_wake_policy=enabled
            ",
        )
        .expect("parse enabled lifecycle policy");

        let disabled = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            system_sleep_wake_policy=disabled
            ",
        )
        .expect("parse disabled lifecycle policy");

        assert_eq!(
            enabled.system_sleep_wake_policy,
            SystemSleepWakePolicy::Enabled
        );
        assert!(enabled.system_sleep_wake_policy.is_enabled());
        assert_eq!(
            disabled.system_sleep_wake_policy,
            SystemSleepWakePolicy::Disabled
        );
        assert!(!disabled.system_sleep_wake_policy.is_enabled());
        assert_eq!(SystemSleepWakePolicy::Enabled.as_str(), "enabled");
        assert_eq!(SystemSleepWakePolicy::Disabled.as_str(), "disabled");
    }

    #[test]
    fn parse_accepts_screen_idle_blank_policy_values() {
        let enabled = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            screen_idle_blank=enabled
            ",
        )
        .expect("parse enabled idle blank policy");

        let disabled = parse_config(
            "\
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            screen_idle_blank=disabled
            ",
        )
        .expect("parse disabled idle blank policy");

        assert_eq!(enabled.screen_idle_blank, ScreenIdleBlankPolicy::Enabled);
        assert!(enabled.screen_idle_blank.is_enabled());
        assert_eq!(disabled.screen_idle_blank, ScreenIdleBlankPolicy::Disabled);
        assert!(!disabled.screen_idle_blank.is_enabled());
        assert_eq!(ScreenIdleBlankPolicy::Enabled.as_str(), "enabled");
        assert_eq!(ScreenIdleBlankPolicy::Disabled.as_str(), "disabled");
    }

    #[test]
    fn parse_uses_last_duplicate_value() {
        let config = parse_config(
            "\
            tv_ip=10.0.0.1
            tv_ip=10.0.0.2
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_4
            ",
        )
        .expect("parse duplicate config");

        assert_eq!(config.tv_ip.to_string(), "10.0.0.2");
    }

    #[test]
    fn parse_rejects_missing_required_keys() {
        let err = parse_config(
            "\
            tv_ip=192.168.1.42
            input=HDMI_1
            ",
        )
        .expect_err("missing tv_mac should fail");

        assert_eq!(
            err.to_string(),
            "missing required config key `tvs_primary_mac`"
        );
    }

    #[test]
    fn parse_rejects_invalid_values() {
        let err = parse_config(
            "\
            tv_ip=not-an-ip
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_1
            ",
        )
        .expect_err("invalid IP should fail");

        assert!(matches!(
            err,
            ConfigError::InvalidValue {
                key: "tvs_primary_ip",
                ..
            }
        ));
    }

    #[test]
    fn parse_ignores_comments_and_unknown_keys() {
        let config = parse_config(
            "\
            # comment
            unused=value
            tv_ip=192.168.1.42
            tv_mac=aa:bb:cc:dd:ee:ff
            input=HDMI_3
            ",
        )
        .expect("parse config with unknown keys");

        assert_eq!(config.input, HdmiInput::Hdmi3);
    }
}
