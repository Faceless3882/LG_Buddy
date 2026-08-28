use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::session_notifications::{
    SessionBusUpdateNotificationHandoff, UpdateNotificationError, UpdateNotificationHandoff,
    UpdateNotificationRequest,
};
use crate::settings::{SettingsError, SettingsStore};
use crate::version::{ReleaseChannel, VersionInfo};

const GITHUB_RELEASES_API_BASE: &str =
    "https://api.github.com/repos/Staphylococcus/LG_Buddy/releases";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_CONNECT_TIMEOUT_SECONDS: u64 = 5;
const GITHUB_REQUEST_TIMEOUT_SECONDS: u64 = 20;
const PRERELEASE_PAGE_SIZE: u8 = 20;
const CACHE_DIR_NAME: &str = "lg-buddy";
const UPDATE_CHECK_CACHE_FILE_NAME: &str = "update-check.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesCommand {
    Check {
        channel: Option<UpdateChannel>,
        notify: bool,
    },
    BackgroundCheck,
}

impl UpdatesCommand {
    pub fn parse<I, S>(args: I) -> Result<Self, UpdatesParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let Some(subcommand) = args.next() else {
            return Err(UpdatesParseError::MissingSubcommand);
        };

        match subcommand.as_ref() {
            "check" => parse_check_args(args),
            "background-check" => parse_background_check_args(args),
            other => Err(UpdatesParseError::UnknownSubcommand(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Check { .. } => "check",
            Self::BackgroundCheck => "background-check",
        }
    }

    fn notify(&self) -> bool {
        match self {
            Self::Check { notify, .. } => *notify,
            Self::BackgroundCheck => true,
        }
    }

    fn check_channel(&self) -> Option<UpdateChannel> {
        match self {
            Self::Check { channel, .. } => *channel,
            Self::BackgroundCheck => None,
        }
    }
}

fn parse_background_check_args<I, S>(args: I) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let extra_args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    if extra_args.is_empty() {
        Ok(UpdatesCommand::BackgroundCheck)
    } else {
        Err(UpdatesParseError::UnexpectedArguments {
            subcommand: "background-check",
            arguments: extra_args,
        })
    }
}

fn parse_check_args<I, S>(args: I) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let mut channel = None;
    let mut notify = false;

    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--channel" => {
                if channel.is_some() {
                    return Err(UpdatesParseError::DuplicateChannel);
                }

                let value = args.next().ok_or(UpdatesParseError::MissingChannelValue)?;
                channel = Some(UpdateChannel::parse(value.as_ref())?);
            }
            "--notify" => {
                if notify {
                    return Err(UpdatesParseError::DuplicateNotify);
                }

                notify = true;
            }
            other => {
                let mut unexpected = vec![other.to_string()];
                unexpected.extend(args.map(|arg| arg.as_ref().to_string()));
                return Err(UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: unexpected,
                });
            }
        }
    }

    Ok(UpdatesCommand::Check { channel, notify })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingChannelValue,
    DuplicateChannel,
    DuplicateNotify,
    UnknownChannel(String),
    UnexpectedArguments {
        subcommand: &'static str,
        arguments: Vec<String>,
    },
}

impl fmt::Display for UpdatesParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => write!(
                f,
                "missing updates command; expected `updates check [--channel stable|prerelease] [--notify]`"
            ),
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown updates command `{subcommand}`")
            }
            Self::MissingChannelValue => write!(f, "missing channel value for `updates check`"),
            Self::DuplicateChannel => write!(f, "duplicate `--channel` option"),
            Self::DuplicateNotify => write!(f, "duplicate `--notify` option"),
            Self::UnknownChannel(channel) => write!(
                f,
                "unknown updates channel `{channel}`; expected stable or prerelease"
            ),
            Self::UnexpectedArguments {
                subcommand,
                arguments,
            } => write!(
                f,
                "unexpected arguments for `updates {subcommand}`: {}",
                arguments.join(" ")
            ),
        }
    }
}

impl Error for UpdatesParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    fn parse(value: &str) -> Result<Self, UpdatesParseError> {
        match value {
            "stable" => Ok(Self::Stable),
            "prerelease" => Ok(Self::Prerelease),
            other => Err(UpdatesParseError::UnknownChannel(other.to_string())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }

    fn default_for(version: VersionInfo) -> Self {
        match version.channel() {
            ReleaseChannel::Prerelease => Self::Prerelease,
            ReleaseChannel::Dev | ReleaseChannel::Stable => Self::Stable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundUpdateChannel {
    Stable,
    Prerelease,
}

impl BackgroundUpdateChannel {
    fn update_channel(self) -> UpdateChannel {
        match self {
            Self::Stable => UpdateChannel::Stable,
            Self::Prerelease => UpdateChannel::Prerelease,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundUpdateCheckPolicy {
    Disabled,
    Enabled { channel: BackgroundUpdateChannel },
}

impl BackgroundUpdateCheckPolicy {
    fn from_settings(store: &SettingsStore) -> Result<Self, UpdatesError> {
        match required_enum_setting(store, "updates.auto_check")? {
            "disabled" => Ok(Self::Disabled),
            "enabled" => Ok(Self::Enabled {
                channel: parse_background_update_channel(store)?,
            }),
            _ => Err(UpdatesError::SettingsInvariant(
                "updates.auto_check resolved to an unsupported value".to_string(),
            )),
        }
    }

    fn check_command(self) -> Option<UpdatesCommand> {
        match self {
            Self::Disabled => None,
            Self::Enabled { channel } => Some(UpdatesCommand::Check {
                channel: Some(channel.update_channel()),
                notify: true,
            }),
        }
    }
}

fn parse_background_update_channel(
    store: &SettingsStore,
) -> Result<BackgroundUpdateChannel, UpdatesError> {
    match required_enum_setting(store, "updates.channel")? {
        "stable" => Ok(BackgroundUpdateChannel::Stable),
        "prerelease" => Ok(BackgroundUpdateChannel::Prerelease),
        _ => Err(UpdatesError::SettingsInvariant(
            "updates.channel resolved to an unsupported value".to_string(),
        )),
    }
}

fn required_enum_setting(
    store: &SettingsStore,
    key: &'static str,
) -> Result<&'static str, UpdatesError> {
    store
        .effective_by_name(key)?
        .required_value()?
        .as_enum()
        .ok_or_else(|| {
            UpdatesError::SettingsInvariant(format!("{key} resolved to a non-enum value"))
        })
}

trait BackgroundUpdateSettings {
    fn background_update_check_policy(&self) -> Result<BackgroundUpdateCheckPolicy, UpdatesError>;
}

#[derive(Debug, Clone, Copy)]
struct EnvBackgroundUpdateSettings;

impl BackgroundUpdateSettings for EnvBackgroundUpdateSettings {
    fn background_update_check_policy(&self) -> Result<BackgroundUpdateCheckPolicy, UpdatesError> {
        let store = SettingsStore::load_from_env()?;
        BackgroundUpdateCheckPolicy::from_settings(&store)
    }
}

#[derive(Debug, Clone, Default)]
struct UpdateCachePathSources<'a> {
    xdg_cache_home: Option<&'a Path>,
    home: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCachePathError {
    NotConfigured,
}

impl fmt::Display for UpdateCachePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "could not resolve an update cache path from XDG_CACHE_HOME or HOME"
            ),
        }
    }
}

impl Error for UpdateCachePathError {}

fn resolve_update_cache_path(
    sources: UpdateCachePathSources<'_>,
) -> Result<PathBuf, UpdateCachePathError> {
    if let Some(path) = sources.xdg_cache_home {
        return Ok(path.join(CACHE_DIR_NAME).join(UPDATE_CHECK_CACHE_FILE_NAME));
    }

    if let Some(path) = sources.home {
        return Ok(path
            .join(".cache")
            .join(CACHE_DIR_NAME)
            .join(UPDATE_CHECK_CACHE_FILE_NAME));
    }

    Err(UpdateCachePathError::NotConfigured)
}

fn resolve_update_cache_path_from_env() -> Result<PathBuf, UpdateCachePathError> {
    let xdg_cache_home = non_empty_env_path("XDG_CACHE_HOME");
    let home = non_empty_env_path("HOME");

    resolve_update_cache_path(UpdateCachePathSources {
        xdg_cache_home: xdg_cache_home.as_deref(),
        home: home.as_deref(),
    })
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[derive(Debug)]
pub enum UpdatesError {
    Http {
        url: String,
        message: String,
    },
    ApiStatus {
        url: String,
        status: u16,
        body: String,
    },
    ApiShape {
        endpoint: &'static str,
        source: serde_json::Error,
    },
    InvalidLocalVersion {
        version: String,
        source: semver::Error,
    },
    NoMatchingRelease {
        channel: UpdateChannel,
    },
    NotModifiedWithoutCache {
        channel: UpdateChannel,
    },
    CachePath(UpdateCachePathError),
    CacheDecode {
        path: PathBuf,
        source: serde_json::Error,
    },
    CacheEncode(serde_json::Error),
    Settings(SettingsError),
    SettingsInvariant(String),
    DeferredFailures(Vec<UpdatesDeferredFailure>),
    Notification(UpdateNotificationError),
    Io(io::Error),
}

#[derive(Debug)]
pub enum UpdatesDeferredFailure {
    Cache(Box<UpdatesError>),
    Notification(UpdateNotificationError),
}

impl fmt::Display for UpdatesDeferredFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cache(err) => write!(f, "update cache failed: {err}"),
            Self::Notification(err) => write!(f, "update notification handoff failed: {err}"),
        }
    }
}

impl Error for UpdatesDeferredFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(err) => Some(err.as_ref()),
            Self::Notification(err) => Some(err),
        }
    }
}

impl fmt::Display for UpdatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { url, message } => {
                write!(f, "could not query GitHub releases API `{url}`: {message}")
            }
            Self::ApiStatus { url, status, body } => {
                if body.trim().is_empty() {
                    write!(
                        f,
                        "GitHub releases API `{url}` returned HTTP status {status}"
                    )
                } else {
                    write!(
                        f,
                        "GitHub releases API `{url}` returned HTTP status {status}: {}",
                        body.trim()
                    )
                }
            }
            Self::ApiShape { endpoint, source } => {
                write!(
                    f,
                    "could not parse GitHub releases API `{endpoint}` response: {source}"
                )
            }
            Self::InvalidLocalVersion { version, source } => {
                write!(f, "invalid local LG Buddy version `{version}`: {source}")
            }
            Self::NoMatchingRelease { channel } => {
                write!(
                    f,
                    "GitHub releases API returned no matching release for {} channel",
                    channel.as_str()
                )
            }
            Self::NotModifiedWithoutCache { channel } => {
                write!(
                    f,
                    "GitHub releases API reported no changes for {} channel, but the local update cache has no usable release metadata",
                    channel.as_str()
                )
            }
            Self::CachePath(err) => write!(f, "{err}"),
            Self::CacheDecode { path, source } => {
                write!(
                    f,
                    "could not parse update check cache `{}`: {source}",
                    path.display()
                )
            }
            Self::CacheEncode(err) => write!(f, "could not encode update check cache: {err}"),
            Self::Settings(err) => write!(f, "could not read update settings: {err}"),
            Self::SettingsInvariant(message) => {
                write!(f, "invalid update settings metadata: {message}")
            }
            Self::DeferredFailures(failures) => {
                write!(f, "update check completed with deferred failure")?;
                if failures.len() != 1 {
                    write!(f, "s")?;
                }
                write!(f, ": ")?;

                for (index, failure) in failures.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{failure}")?;
                }

                Ok(())
            }
            Self::Notification(err) => write!(f, "could not request update notification: {err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for UpdatesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ApiShape { source, .. } => Some(source),
            Self::InvalidLocalVersion { source, .. } => Some(source),
            Self::CachePath(err) => Some(err),
            Self::CacheDecode { source, .. } => Some(source),
            Self::CacheEncode(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::DeferredFailures(failures) => {
                failures.iter().find_map(|failure| failure.source())
            }
            Self::Notification(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Http { .. }
            | Self::ApiStatus { .. }
            | Self::NoMatchingRelease { .. }
            | Self::NotModifiedWithoutCache { .. }
            | Self::SettingsInvariant(_) => None,
        }
    }
}

impl From<io::Error> for UpdatesError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SettingsError> for UpdatesError {
    fn from(value: SettingsError) -> Self {
        Self::Settings(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    version: Version,
    channel: UpdateChannel,
    url: String,
}

impl ReleaseInfo {
    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn channel(&self) -> UpdateChannel {
        self.channel
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn to_cached(&self) -> CachedReleaseInfo {
        CachedReleaseInfo {
            version: self.version.to_string(),
            channel: self.channel,
            url: self.url.clone(),
        }
    }

    fn from_cached(cached: &CachedReleaseInfo) -> Option<Self> {
        Version::parse(&cached.version).ok().map(|version| Self {
            version,
            channel: cached.channel,
            url: cached.url.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateNotificationReason {
    NewRelease,
}

impl UpdateNotificationReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NewRelease => "new release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateNotificationSkipReason {
    NotRequested,
    NoUpdateAvailable,
    AlreadyShownForRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateNotificationDecision {
    Notify {
        reason: UpdateNotificationReason,
    },
    Skip {
        reason: UpdateNotificationSkipReason,
    },
}

#[derive(Debug, Clone, Copy)]
struct UpdateNotificationPolicyInput<'a> {
    notify_requested: bool,
    update_available: bool,
    latest: &'a ReleaseInfo,
    last_notification: Option<&'a CachedUpdateNotification>,
}

fn evaluate_update_notification_policy(
    input: UpdateNotificationPolicyInput<'_>,
) -> UpdateNotificationDecision {
    if !input.notify_requested {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::NotRequested,
        };
    }

    if !input.update_available {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::NoUpdateAvailable,
        };
    }

    if input
        .last_notification
        .is_some_and(|notification| notification.matches_release(input.latest))
    {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::AlreadyShownForRelease,
        };
    }

    UpdateNotificationDecision::Notify {
        reason: UpdateNotificationReason::NewRelease,
    }
}

fn render_update_notification_sent(reason: UpdateNotificationReason) -> String {
    format!("notification: sent ({})\n", reason.as_str())
}

fn render_update_notification_failure(reason: UpdateNotificationReason) -> String {
    format!("notification: failed ({})\n", reason.as_str())
}

fn render_update_notification_skip(
    reason: UpdateNotificationSkipReason,
    latest: &ReleaseInfo,
) -> String {
    let reason = match reason {
        UpdateNotificationSkipReason::NotRequested => "not requested".to_string(),
        UpdateNotificationSkipReason::NoUpdateAvailable => "no update available".to_string(),
        UpdateNotificationSkipReason::AlreadyShownForRelease => {
            format!("already shown for {}", latest.version())
        }
    };

    format!("notification: skipped ({reason})\n")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct UpdateCheckCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stable: Option<CachedUpdateCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prerelease: Option<CachedUpdateCheck>,
}

impl UpdateCheckCache {
    fn entry(&self, channel: UpdateChannel) -> Option<&CachedUpdateCheck> {
        match channel {
            UpdateChannel::Stable => self.stable.as_ref(),
            UpdateChannel::Prerelease => self.prerelease.as_ref(),
        }
    }

    fn entry_mut(&mut self, channel: UpdateChannel) -> Option<&mut CachedUpdateCheck> {
        match channel {
            UpdateChannel::Stable => self.stable.as_mut(),
            UpdateChannel::Prerelease => self.prerelease.as_mut(),
        }
    }

    fn set_entry(&mut self, channel: UpdateChannel, entry: CachedUpdateCheck) {
        match channel {
            UpdateChannel::Stable => self.stable = Some(entry),
            UpdateChannel::Prerelease => self.prerelease = Some(entry),
        }
    }

    fn record_notification(
        &mut self,
        channel: UpdateChannel,
        release: &ReleaseInfo,
        shown_at: u64,
    ) {
        if let Some(entry) = self.entry_mut(channel) {
            entry.last_notification = Some(CachedUpdateNotification {
                shown_at_unix_seconds: shown_at,
                release: release.to_cached(),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedUpdateCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    last_checked_at_unix_seconds: u64,
    latest: CachedReleaseInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_notification: Option<CachedUpdateNotification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedUpdateNotification {
    shown_at_unix_seconds: u64,
    release: CachedReleaseInfo,
}

impl CachedUpdateNotification {
    fn matches_release(&self, release: &ReleaseInfo) -> bool {
        self.release.matches_release(release)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedReleaseInfo {
    version: String,
    channel: UpdateChannel,
    url: String,
}

impl CachedReleaseInfo {
    fn matches_release(&self, release: &ReleaseInfo) -> bool {
        self.version == release.version().to_string()
            && self.channel == release.channel()
            && self.url == release.url()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    check_channel: UpdateChannel,
    current_version: Version,
    current_channel: ReleaseChannel,
    latest: ReleaseInfo,
}

impl UpdateCheckResult {
    pub fn update_available(&self) -> bool {
        self.latest.version > self.current_version
    }

    pub fn render(&self) -> String {
        let status = if self.update_available() {
            "update available"
        } else {
            "up to date"
        };

        format!(
            "status: {status}\ncurrent: {} ({})\nlatest: {} ({})\nurl: {}\n",
            self.current_version,
            self.current_channel.as_str(),
            self.latest.version(),
            self.latest.channel().as_str(),
            self.latest.url()
        )
    }

    fn notification_request(&self) -> Result<UpdateNotificationRequest, UpdateNotificationError> {
        UpdateNotificationRequest::new(
            self.check_channel,
            self.current_version.clone(),
            self.current_channel,
            self.latest.version().clone(),
            self.latest.channel(),
            self.latest.url().to_string(),
        )
    }
}

trait GitHubReleasesClient {
    fn get(
        &self,
        endpoint: ReleaseEndpoint,
        user_agent: &str,
        if_none_match: Option<&str>,
    ) -> Result<GitHubReleaseResponse, UpdatesError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitHubReleaseResponse {
    Ok { body: String, etag: Option<String> },
    NotModified,
}

#[derive(Debug, Clone, Copy)]
enum ReleaseEndpoint {
    LatestStable,
    ReleasesList { per_page: u8 },
}

impl ReleaseEndpoint {
    fn url(self, base: &str) -> String {
        match self {
            Self::LatestStable => format!("{base}/latest"),
            Self::ReleasesList { per_page } => format!("{base}?per_page={per_page}"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LatestStable => "latest",
            Self::ReleasesList { .. } => "releases",
        }
    }
}

struct UreqGitHubReleasesClient {
    base_url: &'static str,
    agent: ureq::Agent,
}

impl Default for UreqGitHubReleasesClient {
    fn default() -> Self {
        Self {
            base_url: GITHUB_RELEASES_API_BASE,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(GITHUB_CONNECT_TIMEOUT_SECONDS))
                .timeout(Duration::from_secs(GITHUB_REQUEST_TIMEOUT_SECONDS))
                .build(),
        }
    }
}

impl GitHubReleasesClient for UreqGitHubReleasesClient {
    fn get(
        &self,
        endpoint: ReleaseEndpoint,
        user_agent: &str,
        if_none_match: Option<&str>,
    ) -> Result<GitHubReleaseResponse, UpdatesError> {
        let url = endpoint.url(self.base_url);
        let mut request = self
            .agent
            .get(&url)
            .set("Accept", GITHUB_ACCEPT)
            .set("User-Agent", user_agent)
            .set("X-GitHub-Api-Version", GITHUB_API_VERSION);

        if let Some(etag) = if_none_match {
            request = request.set("If-None-Match", etag);
        }

        let result = request.call();

        match result {
            Ok(response) => {
                if response.status() == 304 {
                    return Ok(GitHubReleaseResponse::NotModified);
                }

                let etag = response.header("ETag").map(str::to_string);
                response
                    .into_string()
                    .map(|body| GitHubReleaseResponse::Ok { body, etag })
                    .map_err(|err| UpdatesError::Http {
                        url,
                        message: err.to_string(),
                    })
            }
            Err(ureq::Error::Status(304, _)) => Ok(GitHubReleaseResponse::NotModified),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(UpdatesError::ApiStatus { url, status, body })
            }
            Err(ureq::Error::Transport(err)) => Err(UpdatesError::Http {
                url,
                message: err.to_string(),
            }),
        }
    }
}

trait UpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError>;
    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError>;
}

struct FileUpdateCacheStore {
    path: PathBuf,
}

impl FileUpdateCacheStore {
    #[cfg(test)]
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

enum DefaultUpdateCacheStore {
    File(FileUpdateCacheStore),
    Unavailable(UpdateCachePathError),
}

impl DefaultUpdateCacheStore {
    fn from_env() -> Self {
        match resolve_update_cache_path_from_env() {
            Ok(path) => Self::File(FileUpdateCacheStore { path }),
            Err(err) => Self::Unavailable(err),
        }
    }
}

impl UpdateCacheStore for DefaultUpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
        match self {
            Self::File(store) => store.load(),
            Self::Unavailable(_) => Ok(UpdateCheckCache::default()),
        }
    }

    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
        match self {
            Self::File(store) => store.save(cache),
            Self::Unavailable(err) => Err(UpdatesError::CachePath(err.clone())),
        }
    }
}

impl UpdateCacheStore for FileUpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                serde_json::from_str(&contents).map_err(|source| UpdatesError::CacheDecode {
                    path: self.path.clone(),
                    source,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(UpdateCheckCache::default()),
            Err(err) => Err(UpdatesError::Io(err)),
        }
    }

    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
        let contents = serde_json::to_vec_pretty(cache).map_err(UpdatesError::CacheEncode)?;
        atomic_write_file(&self.path, &contents).map_err(UpdatesError::Io)
    }
}

fn atomic_write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut last_error = None;
    for attempt in 0..100 {
        let temp_path = atomic_temp_path(path, attempt);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(err);
                continue;
            }
            Err(err) => return Err(err),
        };

        let result = (|| {
            file.write_all(contents)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp_path, path)
        })();

        if let Err(err) = result {
            let _ = fs::remove_file(&temp_path);
            return Err(err);
        }

        return Ok(());
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create unique update cache temporary file",
        )
    }))
}

fn atomic_temp_path(path: &Path, attempt: u8) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(UPDATE_CHECK_CACHE_FILE_NAME);
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", process::id(), attempt))
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

pub fn run_updates_command<W: io::Write>(
    command: UpdatesCommand,
    writer: &mut W,
) -> Result<(), UpdatesError> {
    let client = UreqGitHubReleasesClient::default();
    let version = VersionInfo::current();
    let notification_handoff = SessionBusUpdateNotificationHandoff;
    let cache_store = DefaultUpdateCacheStore::from_env();

    run_updates_command_with(
        command,
        writer,
        version,
        &client,
        &notification_handoff,
        &cache_store,
        current_unix_seconds(),
    )
}

fn run_updates_command_with<
    W: io::Write,
    C: GitHubReleasesClient,
    N: UpdateNotificationHandoff,
    S: UpdateCacheStore,
>(
    command: UpdatesCommand,
    writer: &mut W,
    version: VersionInfo,
    client: &C,
    notifier: &N,
    cache_store: &S,
    now_unix_seconds: u64,
) -> Result<(), UpdatesError> {
    let background_settings = EnvBackgroundUpdateSettings;
    let context = UpdatesRunContext {
        version,
        client,
        notifier,
        cache_store,
        background_settings: &background_settings,
        now_unix_seconds,
    };
    run_updates_command_with_background_settings(command, writer, context)
}

struct UpdatesRunContext<'a, C, N, S, B> {
    version: VersionInfo,
    client: &'a C,
    notifier: &'a N,
    cache_store: &'a S,
    background_settings: &'a B,
    now_unix_seconds: u64,
}

fn run_updates_command_with_background_settings<
    W: io::Write,
    C: GitHubReleasesClient,
    N: UpdateNotificationHandoff,
    S: UpdateCacheStore,
    B: BackgroundUpdateSettings,
>(
    command: UpdatesCommand,
    writer: &mut W,
    context: UpdatesRunContext<'_, C, N, S, B>,
) -> Result<(), UpdatesError> {
    let command = match command {
        UpdatesCommand::Check { .. } => command,
        UpdatesCommand::BackgroundCheck => {
            let policy = context
                .background_settings
                .background_update_check_policy()?;
            let Some(command) = policy.check_command() else {
                writer.write_all(b"background: skipped (automatic update checks disabled)\n")?;
                return Ok(());
            };
            command
        }
    };
    let notify = command.notify();
    let mut deferred_failures = Vec::new();
    let mut cache = match context.cache_store.load() {
        Ok(cache) => cache,
        Err(err) => {
            deferred_failures.push(UpdatesDeferredFailure::Cache(Box::new(err)));
            UpdateCheckCache::default()
        }
    };
    let result = check_updates_with_cache(
        command.check_channel(),
        context.version,
        context.client,
        &mut cache,
        context.now_unix_seconds,
    )?;

    writer.write_all(result.render().as_bytes())?;
    let notification_decision =
        evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: notify,
            update_available: result.update_available(),
            latest: &result.latest,
            last_notification: cache
                .entry(result.check_channel)
                .and_then(|entry| entry.last_notification.as_ref()),
        });
    match notification_decision {
        UpdateNotificationDecision::Notify { reason } => {
            let notification_result = result
                .notification_request()
                .and_then(|request| context.notifier.show_update_notification(&request));
            match notification_result {
                Ok(_) => {
                    cache.record_notification(
                        result.check_channel,
                        &result.latest,
                        context.now_unix_seconds,
                    );
                    writer.write_all(render_update_notification_sent(reason).as_bytes())?;
                }
                Err(err) => {
                    writer.write_all(render_update_notification_failure(reason).as_bytes())?;
                    deferred_failures.push(UpdatesDeferredFailure::Notification(err));
                }
            }
        }
        UpdateNotificationDecision::Skip { reason } => {
            if notify {
                writer.write_all(
                    render_update_notification_skip(reason, &result.latest).as_bytes(),
                )?;
            }
        }
    }
    if let Err(err) = context.cache_store.save(&cache) {
        deferred_failures.push(UpdatesDeferredFailure::Cache(Box::new(err)));
    }

    if !deferred_failures.is_empty() {
        return Err(UpdatesError::DeferredFailures(deferred_failures));
    }

    Ok(())
}

#[cfg(test)]
fn check_updates<C: GitHubReleasesClient>(
    command: UpdatesCommand,
    current: VersionInfo,
    client: &C,
) -> Result<UpdateCheckResult, UpdatesError> {
    let mut cache = UpdateCheckCache::default();
    check_updates_with_cache(
        command.check_channel(),
        current,
        client,
        &mut cache,
        current_unix_seconds(),
    )
}

fn check_updates_with_cache<C: GitHubReleasesClient>(
    channel: Option<UpdateChannel>,
    current: VersionInfo,
    client: &C,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
) -> Result<UpdateCheckResult, UpdatesError> {
    let channel = channel.unwrap_or_else(|| UpdateChannel::default_for(current));
    let current_version =
        Version::parse(current.version()).map_err(|source| UpdatesError::InvalidLocalVersion {
            version: current.version().to_string(),
            source,
        })?;
    let latest = fetch_latest_release(channel, current, client, cache, now_unix_seconds)?;

    Ok(UpdateCheckResult {
        check_channel: channel,
        current_version,
        current_channel: current.channel(),
        latest,
    })
}

fn fetch_latest_release<C: GitHubReleasesClient>(
    channel: UpdateChannel,
    current: VersionInfo,
    client: &C,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
) -> Result<ReleaseInfo, UpdatesError> {
    let user_agent = format!("lg-buddy/{}", current.version());
    let cached_etag = cache.entry(channel).and_then(|entry| entry.etag.as_deref());

    match channel {
        UpdateChannel::Stable => {
            let endpoint = ReleaseEndpoint::LatestStable;
            let response = client.get(endpoint, &user_agent, cached_etag)?;

            latest_from_response(channel, response, cache, now_unix_seconds, |body| {
                let release: GitHubRelease =
                    serde_json::from_str(body).map_err(|source| UpdatesError::ApiShape {
                        endpoint: endpoint.label(),
                        source,
                    })?;

                release_info_from_api_release(release, channel)
                    .ok_or(UpdatesError::NoMatchingRelease { channel })
            })
        }
        UpdateChannel::Prerelease => {
            let endpoint = ReleaseEndpoint::ReleasesList {
                per_page: PRERELEASE_PAGE_SIZE,
            };
            let response = client.get(endpoint, &user_agent, cached_etag)?;

            latest_from_response(channel, response, cache, now_unix_seconds, |body| {
                let releases: Vec<GitHubRelease> =
                    serde_json::from_str(body).map_err(|source| UpdatesError::ApiShape {
                        endpoint: endpoint.label(),
                        source,
                    })?;

                releases
                    .into_iter()
                    .filter_map(|release| release_info_from_api_release(release, channel))
                    .max_by(|left, right| left.version.cmp(&right.version))
                    .ok_or(UpdatesError::NoMatchingRelease { channel })
            })
        }
    }
}

fn latest_from_response<F>(
    channel: UpdateChannel,
    response: GitHubReleaseResponse,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
    parse_latest: F,
) -> Result<ReleaseInfo, UpdatesError>
where
    F: FnOnce(&str) -> Result<ReleaseInfo, UpdatesError>,
{
    match response {
        GitHubReleaseResponse::Ok { body, etag } => {
            let latest = parse_latest(&body)?;
            let last_notification = cache
                .entry(channel)
                .and_then(|entry| entry.last_notification.clone());
            cache.set_entry(
                channel,
                CachedUpdateCheck {
                    etag,
                    last_checked_at_unix_seconds: now_unix_seconds,
                    latest: latest.to_cached(),
                    last_notification,
                },
            );
            Ok(latest)
        }
        GitHubReleaseResponse::NotModified => {
            let mut entry = cache
                .entry(channel)
                .cloned()
                .ok_or(UpdatesError::NotModifiedWithoutCache { channel })?;
            let latest = ReleaseInfo::from_cached(&entry.latest)
                .ok_or(UpdatesError::NotModifiedWithoutCache { channel })?;
            entry.last_checked_at_unix_seconds = now_unix_seconds;
            cache.set_entry(channel, entry);
            Ok(latest)
        }
    }
}

fn release_info_from_api_release(
    release: GitHubRelease,
    channel: UpdateChannel,
) -> Option<ReleaseInfo> {
    if release.draft {
        return None;
    }

    match channel {
        UpdateChannel::Stable if release.prerelease => return None,
        UpdateChannel::Stable | UpdateChannel::Prerelease => {}
    }

    let release_channel = if release.prerelease {
        UpdateChannel::Prerelease
    } else {
        UpdateChannel::Stable
    };

    parse_release_version(&release.tag_name).map(|version| ReleaseInfo {
        version,
        channel: release_channel,
        url: release.html_url,
    })
}

fn parse_release_version(tag_name: &str) -> Option<Version> {
    Version::parse(tag_name.strip_prefix('v').unwrap_or(tag_name)).ok()
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write_file, check_updates, check_updates_with_cache,
        evaluate_update_notification_policy, parse_release_version, resolve_update_cache_path,
        run_updates_command_with, run_updates_command_with_background_settings,
        BackgroundUpdateChannel, BackgroundUpdateCheckPolicy, BackgroundUpdateSettings,
        CachedReleaseInfo, CachedUpdateCheck, CachedUpdateNotification, DefaultUpdateCacheStore,
        FileUpdateCacheStore, GitHubReleaseResponse, GitHubReleasesClient, ReleaseEndpoint,
        ReleaseInfo, UpdateCachePathError, UpdateCachePathSources, UpdateCacheStore, UpdateChannel,
        UpdateCheckCache, UpdateNotificationDecision, UpdateNotificationPolicyInput,
        UpdateNotificationReason, UpdateNotificationSkipReason, UpdatesCommand,
        UpdatesDeferredFailure, UpdatesError, UpdatesRunContext, UreqGitHubReleasesClient,
        PRERELEASE_PAGE_SIZE,
    };
    use crate::session_notifications::{
        UpdateNotificationError, UpdateNotificationHandoff, UpdateNotificationOutcome,
        UpdateNotificationRequest,
    };
    use crate::settings::{ConfigEnvReader, SettingsStore};
    use crate::version::{ReleaseChannel, VersionInfo};
    use semver::Version;
    use std::cell::RefCell;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::process;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    };
    use std::thread;
    use std::time::Duration;

    const TEST_NOW: u64 = 1_778_234_400;

    #[derive(Debug)]
    struct MockGitHubReleasesClient {
        responses: RefCell<Vec<Result<GitHubReleaseResponse, UpdatesError>>>,
        requests: RefCell<Vec<(String, String, Option<String>)>>,
    }

    impl MockGitHubReleasesClient {
        fn new(responses: Vec<Result<String, UpdatesError>>) -> Self {
            Self::new_responses(
                responses
                    .into_iter()
                    .map(|response| {
                        response.map(|body| GitHubReleaseResponse::Ok { body, etag: None })
                    })
                    .collect(),
            )
        }

        fn new_responses(responses: Vec<Result<GitHubReleaseResponse, UpdatesError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests
                .borrow()
                .iter()
                .map(|(url, user_agent, _)| (url.clone(), user_agent.clone()))
                .collect()
        }

        fn requests_with_etags(&self) -> Vec<(String, String, Option<String>)> {
            self.requests.borrow().clone()
        }
    }

    impl GitHubReleasesClient for MockGitHubReleasesClient {
        fn get(
            &self,
            endpoint: ReleaseEndpoint,
            user_agent: &str,
            if_none_match: Option<&str>,
        ) -> Result<GitHubReleaseResponse, UpdatesError> {
            self.requests.borrow_mut().push((
                endpoint.url("https://api.example.test/releases"),
                user_agent.to_string(),
                if_none_match.map(str::to_string),
            ));
            self.responses.borrow_mut().remove(0)
        }
    }

    #[derive(Debug, Default)]
    struct MemoryUpdateCacheStore {
        cache: RefCell<UpdateCheckCache>,
    }

    impl MemoryUpdateCacheStore {
        fn with_cache(cache: UpdateCheckCache) -> Self {
            Self {
                cache: RefCell::new(cache),
            }
        }

        fn cache(&self) -> UpdateCheckCache {
            self.cache.borrow().clone()
        }
    }

    impl UpdateCacheStore for MemoryUpdateCacheStore {
        fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
            Ok(self.cache())
        }

        fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
            self.cache.replace(cache.clone());
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FailingSaveUpdateCacheStore {
        cache: RefCell<UpdateCheckCache>,
    }

    impl FailingSaveUpdateCacheStore {
        fn cache(&self) -> UpdateCheckCache {
            self.cache.borrow().clone()
        }
    }

    impl UpdateCacheStore for FailingSaveUpdateCacheStore {
        fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
            Ok(self.cache())
        }

        fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
            self.cache.replace(cache.clone());
            Err(UpdatesError::Io(io::Error::other("cache unwritable")))
        }
    }

    struct RecordingNotifier {
        notifications: RefCell<Vec<UpdateNotificationRequest>>,
        result: Result<UpdateNotificationOutcome, UpdateNotificationError>,
    }

    impl RecordingNotifier {
        fn failing(message: &str) -> Self {
            Self {
                notifications: RefCell::new(Vec::new()),
                result: Err(UpdateNotificationError::Transport(message.to_string())),
            }
        }

        fn notifications(&self) -> Vec<UpdateNotificationRequest> {
            self.notifications.borrow().clone()
        }
    }

    impl Default for RecordingNotifier {
        fn default() -> Self {
            Self {
                notifications: RefCell::new(Vec::new()),
                result: Ok(UpdateNotificationOutcome::Sent),
            }
        }
    }

    impl UpdateNotificationHandoff for RecordingNotifier {
        fn show_update_notification(
            &self,
            request: &UpdateNotificationRequest,
        ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
            self.notifications.borrow_mut().push(request.clone());
            self.result.clone()
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct StaticBackgroundUpdateSettings {
        policy: BackgroundUpdateCheckPolicy,
    }

    impl StaticBackgroundUpdateSettings {
        fn enabled(channel: BackgroundUpdateChannel) -> Self {
            Self {
                policy: BackgroundUpdateCheckPolicy::Enabled { channel },
            }
        }

        fn disabled() -> Self {
            Self {
                policy: BackgroundUpdateCheckPolicy::Disabled,
            }
        }
    }

    impl BackgroundUpdateSettings for StaticBackgroundUpdateSettings {
        fn background_update_check_policy(
            &self,
        ) -> Result<BackgroundUpdateCheckPolicy, UpdatesError> {
            Ok(self.policy)
        }
    }

    fn updates_run_context<'a, C, N, S, B>(
        version: VersionInfo,
        client: &'a C,
        notifier: &'a N,
        cache_store: &'a S,
        background_settings: &'a B,
        now_unix_seconds: u64,
    ) -> UpdatesRunContext<'a, C, N, S, B> {
        UpdatesRunContext {
            version,
            client,
            notifier,
            cache_store,
            background_settings,
            now_unix_seconds,
        }
    }

    fn version_info(version: &'static str, channel: ReleaseChannel) -> VersionInfo {
        VersionInfo::for_testing(version, channel, Some("test"))
    }

    fn stable_release(tag: &str) -> String {
        release_json(tag, false, false)
    }

    fn prerelease(tag: &str) -> String {
        release_json(tag, false, true)
    }

    fn draft_prerelease(tag: &str) -> String {
        release_json(tag, true, true)
    }

    fn release_json(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.test/releases/tag/{tag}","draft":{draft},"prerelease":{prerelease}}}"#
        )
    }

    fn release_info(version: &str, channel: UpdateChannel, url: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse(version).expect("test version should parse"),
            channel,
            url: url.to_string(),
        }
    }

    fn api_response(body: String, etag: Option<&str>) -> GitHubReleaseResponse {
        GitHubReleaseResponse::Ok {
            body,
            etag: etag.map(str::to_string),
        }
    }

    fn cached_entry(
        etag: Option<&str>,
        version: &str,
        channel: UpdateChannel,
        url: &str,
        last_checked_at_unix_seconds: u64,
    ) -> CachedUpdateCheck {
        CachedUpdateCheck {
            etag: etag.map(str::to_string),
            last_checked_at_unix_seconds,
            latest: CachedReleaseInfo {
                version: version.to_string(),
                channel,
                url: url.to_string(),
            },
            last_notification: None,
        }
    }

    fn cached_notification(
        version: &str,
        channel: UpdateChannel,
        url: &str,
        shown_at_unix_seconds: u64,
    ) -> CachedUpdateNotification {
        CachedUpdateNotification {
            shown_at_unix_seconds,
            release: CachedReleaseInfo {
                version: version.to_string(),
                channel,
                url: url.to_string(),
            },
        }
    }

    fn cached_entry_with_notification(
        etag: Option<&str>,
        version: &str,
        channel: UpdateChannel,
        url: &str,
        last_checked_at_unix_seconds: u64,
        notification_shown_at_unix_seconds: u64,
    ) -> CachedUpdateCheck {
        let mut entry = cached_entry(etag, version, channel, url, last_checked_at_unix_seconds);
        entry.last_notification = Some(cached_notification(
            version,
            channel,
            url,
            notification_shown_at_unix_seconds,
        ));
        entry
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    fn check(channel: Option<UpdateChannel>) -> UpdatesCommand {
        UpdatesCommand::Check {
            channel,
            notify: false,
        }
    }

    fn check_notify(channel: Option<UpdateChannel>) -> UpdatesCommand {
        UpdatesCommand::Check {
            channel,
            notify: true,
        }
    }

    fn background_check() -> UpdatesCommand {
        UpdatesCommand::BackgroundCheck
    }

    #[test]
    fn notification_policy_skips_when_notification_was_not_requested() {
        let latest = release_info(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: false,
            update_available: true,
            latest: &latest,
            last_notification: None,
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::NotRequested
            }
        );
    }

    #[test]
    fn notification_policy_skips_when_no_update_is_available() {
        let latest = release_info(
            "1.1.0",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.0",
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: false,
            latest: &latest,
            last_notification: None,
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::NoUpdateAvailable
            }
        );
    }

    #[test]
    fn notification_policy_skips_when_latest_release_was_already_shown() {
        let latest = release_info(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        );
        let last_notification = cached_notification(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
            TEST_NOW - 1,
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: true,
            latest: &latest,
            last_notification: Some(&last_notification),
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::AlreadyShownForRelease
            }
        );
    }

    #[test]
    fn notification_policy_notifies_when_latest_release_has_not_been_shown() {
        let latest = release_info(
            "1.1.2",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.2",
        );
        let last_notification = cached_notification(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
            TEST_NOW - 1,
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: true,
            latest: &latest,
            last_notification: Some(&last_notification),
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Notify {
                reason: UpdateNotificationReason::NewRelease
            }
        );
    }

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(label: &str) -> PathBuf {
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-updates-{label}-{}-{counter}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("create test temp dir");
        path
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn cache_path_resolver_prefers_xdg_cache_home() {
        let xdg_cache_home = PathBuf::from("/tmp/xdg-cache");
        let home = PathBuf::from("/home/test-user");

        let path = resolve_update_cache_path(UpdateCachePathSources {
            xdg_cache_home: Some(&xdg_cache_home),
            home: Some(&home),
        })
        .expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/tmp/xdg-cache/lg-buddy/update-check.json")
        );
    }

    #[test]
    fn cache_path_resolver_falls_back_to_home_cache() {
        let home = PathBuf::from("/home/test-user");

        let path = resolve_update_cache_path(UpdateCachePathSources {
            xdg_cache_home: None,
            home: Some(&home),
        })
        .expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/home/test-user/.cache/lg-buddy/update-check.json")
        );
    }

    #[test]
    fn empty_env_paths_are_treated_as_unset_for_cache_resolution() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_xdg_cache_home = std::env::var_os("XDG_CACHE_HOME");
        let original_home = std::env::var_os("HOME");

        std::env::set_var("XDG_CACHE_HOME", "");
        std::env::set_var("HOME", "/home/test-user");

        let path = super::resolve_update_cache_path_from_env().expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/home/test-user/.cache/lg-buddy/update-check.json")
        );

        match original_xdg_cache_home {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn missing_cache_loads_as_empty_and_malformed_cache_reports_decode_error() {
        let dir = unique_temp_dir("malformed-cache");
        let path = dir.join("lg-buddy").join("update-check.json");
        let store = FileUpdateCacheStore::new(path.clone());

        assert_eq!(
            store.load().expect("missing cache should load"),
            UpdateCheckCache::default()
        );

        fs::create_dir_all(path.parent().expect("cache path parent")).expect("create cache dir");
        fs::write(&path, "{").expect("write malformed cache");

        let err = store
            .load()
            .expect_err("malformed cache should report decode error");

        assert!(
            matches!(err, UpdatesError::CacheDecode { path: error_path, .. } if error_path == path)
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn file_cache_round_trips_entries_and_preserves_other_channel() {
        let dir = unique_temp_dir("cache-roundtrip");
        let path = dir.join("lg-buddy").join("update-check.json");
        let store = FileUpdateCacheStore::new(path);

        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry_with_notification(
                Some("\"stable-etag\""),
                "1.1.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.0",
                TEST_NOW,
                TEST_NOW + 1,
            ),
        );
        cache.set_entry(
            UpdateChannel::Prerelease,
            cached_entry(
                Some("\"prerelease-etag\""),
                "1.2.0-beta.1",
                UpdateChannel::Prerelease,
                "https://github.test/releases/tag/v1.2.0-beta.1",
                TEST_NOW + 1,
            ),
        );

        store.save(&cache).expect("save cache");
        assert_eq!(store.load().expect("load cache"), cache);

        let mut updated = store.load().expect("load cache for update");
        updated.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag-2\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW + 2,
            ),
        );
        store.save(&updated).expect("save updated cache");

        let loaded = store.load().expect("load updated cache");
        assert_eq!(
            loaded.entry(UpdateChannel::Stable),
            updated.entry(UpdateChannel::Stable)
        );
        assert_eq!(
            loaded.entry(UpdateChannel::Prerelease),
            cache.entry(UpdateChannel::Prerelease)
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn cache_without_notification_state_loads_with_absent_notification() {
        let cache: UpdateCheckCache = serde_json::from_str(
            r#"{
              "stable": {
                "etag": "\"stable-etag\"",
                "last_checked_at_unix_seconds": 1778234400,
                "latest": {
                  "version": "1.1.0",
                  "channel": "stable",
                  "url": "https://github.test/releases/tag/v1.1.0"
                }
              }
            }"#,
        )
        .expect("legacy cache should decode");

        assert_eq!(
            cache
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification,
            None
        );
    }

    #[test]
    fn failed_atomic_write_does_not_replace_existing_target() {
        let dir = unique_temp_dir("atomic-write-failure");
        let path = dir.join("update-check.json");
        fs::create_dir_all(&path).expect("create directory at target path");

        let err = atomic_write_file(&path, b"{}").expect_err("rename over directory should fail");

        assert!(err.kind() != io::ErrorKind::NotFound);
        assert!(path.is_dir());

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn update_check_sends_cached_etag_and_stores_response_etag() {
        let client = MockGitHubReleasesClient::new_responses(vec![Ok(api_response(
            stable_release("v1.2.0"),
            Some("\"next-etag\""),
        ))]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"cached-etag\""),
                "1.1.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.0",
                TEST_NOW - 1,
            ),
        );

        let result = check_updates_with_cache(
            Some(UpdateChannel::Stable),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                Some("\"cached-etag\"".to_string())
            )]
        );
        let entry = cache
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry");
        assert_eq!(entry.etag.as_deref(), Some("\"next-etag\""));
        assert_eq!(entry.last_checked_at_unix_seconds, TEST_NOW);
        assert_eq!(entry.latest.version, "1.2.0");
    }

    #[test]
    fn ureq_client_maps_not_modified_status_to_cached_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("read local test address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client connection");
            let mut buffer = [0; 2048];
            let length = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..length]);

            assert!(request.starts_with("GET /releases?per_page=20 "));
            assert!(request.contains("If-None-Match: \"cached-etag\""));

            stream
                .write_all(
                    b"HTTP/1.1 304 Not Modified\r\nETag: \"cached-etag\"\r\nContent-Length: 0\r\n\r\n",
                )
                .expect("write response");
        });
        let base_url = Box::leak(format!("http://{address}/releases").into_boxed_str());
        let client = UreqGitHubReleasesClient {
            base_url,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build(),
        };

        let response = client
            .get(
                ReleaseEndpoint::ReleasesList {
                    per_page: PRERELEASE_PAGE_SIZE,
                },
                "lg-buddy/1.1.0-alpha.0",
                Some("\"cached-etag\""),
            )
            .expect("304 response should succeed");

        assert_eq!(response, GitHubReleaseResponse::NotModified);
        server.join().expect("server thread should finish");
    }

    #[test]
    fn stable_not_modified_uses_cached_release_metadata() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW - 10,
            ),
        );

        let result = check_updates_with_cache(
            Some(UpdateChannel::Stable),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("cached stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.1.0 (stable)\nlatest: 1.1.1 (stable)\nurl: https://github.test/releases/tag/v1.1.1\n"
        );
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                Some("\"stable-etag\"".to_string())
            )]
        );
        let entry = cache
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry");
        assert_eq!(entry.etag.as_deref(), Some("\"stable-etag\""));
        assert_eq!(entry.last_checked_at_unix_seconds, TEST_NOW);
    }

    #[test]
    fn prerelease_not_modified_can_use_cached_stable_latest_release() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Prerelease,
            cached_entry(
                Some("\"prerelease-etag\""),
                "1.2.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.2.0",
                TEST_NOW - 10,
            ),
        );

        let result = check_updates_with_cache(
            Some(UpdateChannel::Prerelease),
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("cached prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.channel(), UpdateChannel::Stable);
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases?per_page=20".to_string(),
                "lg-buddy/1.2.0-beta.1".to_string(),
                Some("\"prerelease-etag\"".to_string())
            )]
        );
    }

    #[test]
    fn not_modified_without_cached_release_metadata_is_reported() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();

        let err = check_updates_with_cache(
            Some(UpdateChannel::Stable),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect_err("304 without cache should fail");

        assert!(matches!(
            err,
            UpdatesError::NotModifiedWithoutCache {
                channel: UpdateChannel::Stable
            }
        ));
    }

    #[test]
    fn manual_update_check_reuses_cache_on_not_modified() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check(Some(UpdateChannel::Stable)),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial update check should succeed");
        run_updates_command_with(
            check(Some(UpdateChannel::Stable)),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("cached update check should succeed");

        assert_eq!(rendered(&first_output), rendered(&second_output));
        assert_eq!(
            client.requests_with_etags(),
            vec![
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    None
                ),
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    Some("\"stable-etag\"".to_string())
                )
            ]
        );
        let cache = cache_store.cache();
        assert_eq!(
            cache
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_checked_at_unix_seconds,
            TEST_NOW + 1
        );
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn background_update_check_skips_without_github_or_cache_when_disabled() {
        let client = MockGitHubReleasesClient::new_responses(vec![]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let background_settings = StaticBackgroundUpdateSettings::disabled();
        let mut output = Vec::new();

        run_updates_command_with_background_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &background_settings,
                TEST_NOW,
            ),
        )
        .expect("disabled background update check should succeed");

        assert_eq!(
            rendered(&output),
            "background: skipped (automatic update checks disabled)\n"
        );
        assert!(client.requests_with_etags().is_empty());
        assert!(notifier.notifications().is_empty());
        assert_eq!(cache_store.cache(), UpdateCheckCache::default());
    }

    #[test]
    fn background_update_check_uses_default_stable_channel_and_notifies() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let background_settings =
            StaticBackgroundUpdateSettings::enabled(BackgroundUpdateChannel::Stable);
        let mut output = Vec::new();

        run_updates_command_with_background_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &background_settings,
                TEST_NOW,
            ),
        )
        .expect("enabled background update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn disabled_background_update_check_ignores_invalid_channel_setting() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/tmp/config.env",
            "updates_auto_check=disabled\nupdates_channel=bogus\n",
        ));

        let policy = BackgroundUpdateCheckPolicy::from_settings(&store)
            .expect("disabled background checks should not parse the channel setting");

        assert_eq!(policy.check_command(), None);
    }

    #[test]
    fn enabled_background_update_check_defaults_to_stable_channel() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/tmp/config.env",
            "updates_auto_check=enabled\n",
        ));

        let policy = BackgroundUpdateCheckPolicy::from_settings(&store)
            .expect("enabled background checks should use the settings default channel");

        assert_eq!(
            policy.check_command(),
            Some(UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
                notify: true,
            })
        );
    }

    #[test]
    fn background_update_check_uses_configured_prerelease_channel() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{}]",
            stable_release("v1.1.0"),
            prerelease("v1.2.0-beta.1")
        ))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let background_settings =
            StaticBackgroundUpdateSettings::enabled(BackgroundUpdateChannel::Prerelease);
        let mut output = Vec::new();

        run_updates_command_with_background_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &background_settings,
                TEST_NOW,
            ),
        )
        .expect("configured prerelease background update check should succeed");

        assert!(rendered(&output).contains("latest: 1.2.0-beta.1 (prerelease)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=20".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn background_update_check_reuses_notification_policy_for_repeated_release() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let background_settings =
            StaticBackgroundUpdateSettings::enabled(BackgroundUpdateChannel::Stable);
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with_background_settings(
            background_check(),
            &mut first_output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &background_settings,
                TEST_NOW,
            ),
        )
        .expect("initial background update check should succeed");
        run_updates_command_with_background_settings(
            background_check(),
            &mut second_output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &background_settings,
                TEST_NOW + 1,
            ),
        )
        .expect("repeated background update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(
            rendered(&second_output).contains("notification: skipped (already shown for 1.1.1)")
        );
        assert_eq!(notifier.notifications().len(), 1);
    }

    #[test]
    fn unavailable_cache_path_does_not_block_update_check_but_fails_after_result() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = DefaultUpdateCacheStore::Unavailable(UpdateCachePathError::NotConfigured);
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("unavailable cache path should be reported after update check");

        assert!(rendered(&output).contains("status: update available"));
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                None
            )]
        );
        assert!(notifier.notifications().is_empty());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::CachePath(UpdateCachePathError::NotConfigured))
        ));
    }

    #[test]
    fn malformed_cache_does_not_block_update_check_but_fails_after_result() {
        let dir = unique_temp_dir("malformed-cache-command");
        let path = dir.join("lg-buddy").join("update-check.json");
        fs::create_dir_all(path.parent().expect("cache path parent")).expect("create cache dir");
        fs::write(&path, "{").expect("write malformed cache");

        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = FileUpdateCacheStore::new(path.clone());
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("malformed cache should be reported after update check");

        assert!(rendered(&output).contains("status: update available"));
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                None
            )]
        );
        assert!(notifier.notifications().is_empty());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache decode failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::CacheDecode { path: error_path, .. } if error_path == &path)
        ));
        assert_eq!(
            cache_store
                .load()
                .expect("successful check should replace malformed cache")
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .latest
                .version
                .as_str(),
            "1.1.1"
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn cache_save_failure_does_not_block_requested_notification_but_fails_afterwards() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = FailingSaveUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("cache save failure should be reported after notification");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::Io(_))
        ));
    }

    #[test]
    fn notification_and_cache_save_failures_are_reported_together() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::failing("bus unavailable");
        let cache_store = FailingSaveUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("notification and cache failures should both be reported");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: failed (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert!(cache_store
            .cache()
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry")
            .last_notification
            .is_none());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred failures");
        };
        assert_eq!(failures.len(), 2);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Notification(_)
        ));
        assert!(matches!(
            &failures[1],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::Io(_))
        ));
    }

    #[test]
    fn notify_sends_notification_when_cached_not_modified_update_is_available() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let notifier = RecordingNotifier::default();
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW - 10,
            ),
        );
        let cache_store = MemoryUpdateCacheStore::with_cache(cache);
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying cached update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_cached_not_modified_is_up_to_date() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let notifier = RecordingNotifier::default();
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.0",
                TEST_NOW - 10,
            ),
        );
        let cache_store = MemoryUpdateCacheStore::with_cache(cache);
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying cached update check should succeed");

        assert!(rendered(&output).contains("status: up to date"));
        assert!(rendered(&output).contains("notification: skipped (no update available)"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn updates_check_uses_stable_channel_for_stable_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);

        let result = check_updates(
            check(None),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.1.0 (stable)\nlatest: 1.1.1 (stable)\nurl: https://github.test/releases/tag/v1.1.1\n"
        );
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn updates_check_uses_prerelease_channel_for_prerelease_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{}]",
            stable_release("v1.1.0"),
            prerelease("v1.2.0-beta.1")
        ))]);

        let result = check_updates(
            check(None),
            version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.channel(), UpdateChannel::Prerelease);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=20".to_string(),
                "lg-buddy/1.1.0-beta.1".to_string()
            )]
        );
    }

    #[test]
    fn updates_check_uses_stable_channel_for_dev_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.0"))]);

        let result = check_updates(
            check(None),
            version_info("1.1.0", ReleaseChannel::Dev),
            &client,
        )
        .expect("dev update check should succeed");

        assert!(!result.update_available());
        assert_eq!(
            result.render(),
            "status: up to date\ncurrent: 1.1.0 (dev)\nlatest: 1.1.0 (stable)\nurl: https://github.test/releases/tag/v1.1.0\n"
        );
    }

    #[test]
    fn plain_updates_check_does_not_send_notification() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check(None),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("plain update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn notify_sends_notification_when_update_is_available() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        let notifications = notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].to_dbus_fields(),
            (
                "stable".to_string(),
                "1.1.0".to_string(),
                "stable".to_string(),
                "1.1.1".to_string(),
                "stable".to_string(),
                "https://github.test/releases/tag/v1.1.1".to_string()
            )
        );
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
    }

    #[test]
    fn notify_skips_repeated_notification_for_same_cached_release() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial notifying update check should succeed");
        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("repeated notifying update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(
            rendered(&second_output).contains("notification: skipped (already shown for 1.1.1)")
        );
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests_with_etags(),
            vec![
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    None
                ),
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    Some("\"stable-etag\"".to_string())
                )
            ]
        );
    }

    #[test]
    fn notify_sends_again_when_a_newer_release_is_available() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag-1\""),
            )),
            Ok(api_response(
                stable_release("v1.1.2"),
                Some("\"stable-etag-2\""),
            )),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial notifying update check should succeed");
        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("newer release notifying update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(rendered(&second_output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 2);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.2"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_up_to_date() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.0"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying update check should succeed");

        assert!(rendered(&output).contains("status: up to date"));
        assert!(rendered(&output).contains("notification: skipped (no update available)"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn notify_failure_after_available_update_returns_error_after_rendering_output() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::failing("bus unavailable");
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("notification failure should fail notifying update check");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: failed (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert!(cache_store
            .cache()
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry")
            .last_notification
            .is_none());
        let UpdatesError::DeferredFailures(failures) = &err else {
            panic!("expected deferred notification failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Notification(_)
        ));
        assert_eq!(
            err.to_string(),
            "update check completed with deferred failure: update notification handoff failed: could not request update notification from LG Buddy session service: bus unavailable"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_update_check_fails() {
        let client = MockGitHubReleasesClient::new(vec![Err(UpdatesError::ApiStatus {
            url: "https://api.example.test/releases/latest".to_string(),
            status: 500,
            body: "server error".to_string(),
        })]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(Some(UpdateChannel::Stable)),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("API failure should fail before notification");

        assert!(matches!(err, UpdatesError::ApiStatus { .. }));
        assert!(rendered(&output).is_empty());
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn explicit_stable_channel_reports_up_to_date_for_equal_or_older_versions() {
        for tag in ["v1.1.0", "v1.0.9"] {
            let client = MockGitHubReleasesClient::new(vec![Ok(stable_release(tag))]);

            let result = check_updates(
                check(Some(UpdateChannel::Stable)),
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
            )
            .expect("stable update check should succeed");

            assert!(!result.update_available(), "{tag} should not be newer");
        }
    }

    #[test]
    fn stable_channel_uses_github_latest_endpoint_for_stable_only_ordering() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.2.0"))]);

        let result = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.version().to_string(), "1.2.0");
        assert_eq!(result.latest.channel(), UpdateChannel::Stable);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn prerelease_channel_includes_stable_releases_and_picks_highest_semver() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{},{},{}]",
            draft_prerelease("v1.3.0-beta.1"),
            stable_release("v1.2.0"),
            prerelease("release-0.6"),
            prerelease("v1.2.0-beta.2")
        ))]);

        let result = check_updates(
            check(Some(UpdateChannel::Prerelease)),
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.2.0-beta.1 (prerelease)\nlatest: 1.2.0 (stable)\nurl: https://github.test/releases/tag/v1.2.0\n"
        );
    }

    #[test]
    fn prerelease_channel_uses_semver_ordering_across_release_stages() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{},{},{},{}]",
            stable_release("v1.2.0"),
            prerelease("v1.2.0-rc.1"),
            prerelease("v1.3.0-alpha.1"),
            prerelease("v1.2.0-beta.1"),
            prerelease("v1.2.0-alpha.1")
        ))]);

        let result = check_updates(
            check(Some(UpdateChannel::Prerelease)),
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.version().to_string(), "1.3.0-alpha.1");
        assert_eq!(result.latest.channel(), UpdateChannel::Prerelease);
        assert_eq!(
            result.latest.url(),
            "https://github.test/releases/tag/v1.3.0-alpha.1"
        );
    }

    #[test]
    fn stable_channel_rejects_prerelease_response() {
        let client = MockGitHubReleasesClient::new(vec![Ok(prerelease("v1.1.1-beta.1"))]);

        let err = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("prerelease latest response should fail stable check");

        assert!(matches!(
            err,
            UpdatesError::NoMatchingRelease {
                channel: UpdateChannel::Stable
            }
        ));
    }

    #[test]
    fn api_errors_are_reported() {
        let client = MockGitHubReleasesClient::new(vec![Err(UpdatesError::ApiStatus {
            url: "https://api.example.test/releases/latest".to_string(),
            status: 500,
            body: "server error".to_string(),
        })]);

        let err = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("HTTP status should fail update check");

        assert_eq!(
            err.to_string(),
            "GitHub releases API `https://api.example.test/releases/latest` returned HTTP status 500: server error"
        );
    }

    #[test]
    fn malformed_json_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![Ok("{".to_string())]);

        let err = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("malformed JSON should fail update check");

        assert!(matches!(
            err,
            UpdatesError::ApiShape {
                endpoint: "latest",
                ..
            }
        ));
    }

    #[test]
    fn missing_required_release_fields_are_reported() {
        let client = MockGitHubReleasesClient::new(vec![Ok(
            r#"{"tag_name":"v1.1.1","draft":false,"prerelease":false}"#.to_string(),
        )]);

        let err = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("missing html_url should fail update check");

        assert!(matches!(
            err,
            UpdatesError::ApiShape {
                endpoint: "latest",
                ..
            }
        ));
    }

    #[test]
    fn missing_release_candidate_for_prerelease_channel_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{}]",
            prerelease("release-0.6"),
            draft_prerelease("v1.2.0-beta.1")
        ))]);

        let err = check_updates(
            check(Some(UpdateChannel::Prerelease)),
            version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect_err("missing prerelease candidate should fail update check");

        assert!(matches!(
            err,
            UpdatesError::NoMatchingRelease {
                channel: UpdateChannel::Prerelease
            }
        ));
    }

    #[test]
    fn invalid_local_version_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![]);

        let err = check_updates(
            check(Some(UpdateChannel::Stable)),
            version_info("not-semver", ReleaseChannel::Dev),
            &client,
        )
        .expect_err("invalid local version should fail update check");

        assert!(matches!(err, UpdatesError::InvalidLocalVersion { .. }));
        assert!(client.requests().is_empty());
    }

    #[test]
    fn release_version_parser_accepts_leading_v_and_rejects_legacy_tags() {
        assert_eq!(
            parse_release_version("v1.1.0")
                .expect("leading-v version should parse")
                .to_string(),
            "1.1.0"
        );
        assert!(parse_release_version("release-0.6").is_none());
    }
}
