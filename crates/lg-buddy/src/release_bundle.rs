use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use flate2::read::MultiGzDecoder;
use semver::Version;
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};
use url::Url;

use crate::updates::{ReleaseAsset, ReleaseInfo, UpdateChannel};

const REPOSITORY_OWNER: &str = "Staphylococcus";
const REPOSITORY_NAME: &str = "LG_Buddy";
const GITHUB_API_ROOT: &str = "https://api.github.com";
const GITHUB_RELEASE_ASSET_HOST: &str = "release-assets.githubusercontent.com";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_JSON_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_ASSET_ACCEPT: &str = "application/octet-stream";
const GITHUB_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const GITHUB_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RELEASE_TARGET: &str = "x86_64-unknown-linux-musl";
const CHECKSUM_ASSET_NAME: &str = "sha256sums.txt";
const MANIFEST_NAME: &str = "release-manifest.json";
const ACQUISITION_DIR_NAME: &str = "release-bundles";
const LOCK_FILE_NAME: &str = ".acquisition.lock";
const MAX_TAG_DEPTH: usize = 4;
const MAX_API_BYTES: u64 = 256 * 1024;
const MAX_API_ERROR_BYTES: u64 = 16 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4_096;
const MAX_ARCHIVE_FILES: usize = 2_048;
const MAX_ARCHIVE_PATH_BYTES: usize = 4_096;
const MAX_ARCHIVE_METADATA_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_TRAILING_BYTES: u64 = 1024 * 1024;

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    release_tag: String,
    version: Version,
    channel: UpdateChannel,
    target: String,
    commit: String,
}

impl ReleaseIdentity {
    pub(crate) fn from_parts(
        release_tag: impl Into<String>,
        version: Version,
        channel: UpdateChannel,
        target: impl Into<String>,
        commit: impl Into<String>,
    ) -> Self {
        Self {
            release_tag: release_tag.into(),
            version,
            channel,
            target: target.into(),
            commit: commit.into(),
        }
    }

    pub fn release_tag(&self) -> &str {
        &self.release_tag
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn channel(&self) -> UpdateChannel {
        self.channel
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }
}

#[derive(Debug)]
pub struct VerifiedReleaseBundle {
    root: PathBuf,
    identity: ReleaseIdentity,
    _staging: StagingDirectory,
}

impl VerifiedReleaseBundle {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn identity(&self) -> &ReleaseIdentity {
        &self.identity
    }
}

#[derive(Debug)]
pub enum BundleAcquisitionError {
    CachePathUnavailable,
    UnsafeStaging(String),
    ConcurrentAcquisition,
    ReleaseMetadata(String),
    TagResolution(String),
    Http {
        url: String,
        message: String,
    },
    HttpStatus {
        url: String,
        status: u16,
        body: String,
    },
    ResponseTooLarge {
        label: String,
        max_bytes: u64,
    },
    InterruptedAsset {
        name: String,
        expected: u64,
        actual: u64,
    },
    Digest(String),
    Checksum(String),
    Archive(String),
    Manifest(String),
    Binary(String),
    Io(io::Error),
}

impl fmt::Display for BundleAcquisitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CachePathUnavailable => write!(
                f,
                "could not resolve release-bundle staging from XDG_CACHE_HOME or HOME"
            ),
            Self::UnsafeStaging(message) => {
                write!(f, "release-bundle staging is unsafe: {message}")
            }
            Self::ConcurrentAcquisition => {
                write!(f, "another release-bundle acquisition is already running")
            }
            Self::ReleaseMetadata(message) => write!(f, "invalid release metadata: {message}"),
            Self::TagResolution(message) => write!(f, "could not resolve release tag: {message}"),
            Self::Http { url, message } => write!(f, "request to `{url}` failed: {message}"),
            Self::HttpStatus { url, status, body } => {
                if body.trim().is_empty() {
                    write!(f, "request to `{url}` returned HTTP status {status}")
                } else {
                    write!(
                        f,
                        "request to `{url}` returned HTTP status {status}: {}",
                        body.trim()
                    )
                }
            }
            Self::ResponseTooLarge { label, max_bytes } => {
                write!(f, "{label} exceeded the {max_bytes}-byte limit")
            }
            Self::InterruptedAsset {
                name,
                expected,
                actual,
            } => write!(
                f,
                "release asset `{name}` ended after {actual} bytes; metadata declares {expected}"
            ),
            Self::Digest(message) => write!(f, "release asset digest check failed: {message}"),
            Self::Checksum(message) => write!(f, "published checksum check failed: {message}"),
            Self::Archive(message) => write!(f, "release archive check failed: {message}"),
            Self::Manifest(message) => write!(f, "release manifest check failed: {message}"),
            Self::Binary(message) => write!(f, "bundled binary identity check failed: {message}"),
            Self::Io(err) => write!(f, "release-bundle acquisition failed: {err}"),
        }
    }
}

impl Error for BundleAcquisitionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for BundleAcquisitionError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn acquire_release_bundle(
    release: &ReleaseInfo,
) -> Result<VerifiedReleaseBundle, BundleAcquisitionError> {
    let cache_root = acquisition_cache_root_from_env()?;
    let source = UreqGitHubSource::new();
    acquire_release_bundle_with(release, &cache_root, &source, &EmbeddedBinaryIdentityReader)
}

pub(crate) fn resolve_release_identity(
    release: &ReleaseInfo,
) -> Result<ReleaseIdentity, BundleAcquisitionError> {
    let source = UreqGitHubSource::new();
    resolve_release_with(release, &source).map(|resolved| resolved.identity)
}

pub(crate) fn verify_release_binary_identity(
    binary: &Path,
    expected: &ReleaseIdentity,
) -> Result<ReleaseIdentity, BundleAcquisitionError> {
    let observed =
        read_embedded_binary_identity(binary, expected.target(), expected.release_tag())?;
    if observed != *expected {
        return Err(BundleAcquisitionError::Binary(format!(
            "identity {observed:?} does not match verified release identity {expected:?}"
        )));
    }
    Ok(observed)
}

fn acquisition_cache_root_from_env() -> Result<PathBuf, BundleAcquisitionError> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })
        .ok_or(BundleAcquisitionError::CachePathUnavailable)?;
    Ok(base.join("lg-buddy").join(ACQUISITION_DIR_NAME))
}

fn acquire_release_bundle_with<S: GitHubSource, B: BinaryIdentityReader>(
    release: &ReleaseInfo,
    cache_root: &Path,
    source: &S,
    binary_reader: &B,
) -> Result<VerifiedReleaseBundle, BundleAcquisitionError> {
    let staging = StagingDirectory::create(cache_root)?;
    let resolved = resolve_release_with(release, source)?;
    let fresh_release = resolved.release;
    let expected = resolved.identity;
    let selected = select_release_assets(&fresh_release)?;

    let checksum_path = staging.path.join(CHECKSUM_ASSET_NAME);
    let archive_path = staging.path.join(selected.archive.name());

    let checksum_download =
        source.download_asset(selected.checksum, &checksum_path, MAX_CHECKSUM_BYTES)?;
    pin_download_file(&checksum_path, &checksum_download.file)?;
    verify_download(selected.checksum, &checksum_download.facts)?;

    let archive_download =
        source.download_asset(selected.archive, &archive_path, MAX_ARCHIVE_BYTES)?;
    pin_download_file(&archive_path, &archive_download.file)?;
    verify_download(selected.archive, &archive_download.facts)?;

    let published_digest =
        published_archive_digest(&checksum_download.file, selected.archive.name())?;
    let github_archive_digest = required_github_digest(selected.archive)?;
    if published_digest != github_archive_digest {
        return Err(BundleAcquisitionError::Checksum(format!(
            "archive digest `{published_digest}` does not agree with GitHub digest `{github_archive_digest}`"
        )));
    }

    let plan = inspect_archive(&archive_download.file, selected.archive.name(), &expected)?;
    let root = extract_archive(&archive_download.file, &staging.path, &plan)?;
    let observed = binary_reader.read_identity(
        &root.join("lg-buddy"),
        RELEASE_TARGET,
        fresh_release.tag_name(),
    )?;
    if observed != expected {
        return Err(BundleAcquisitionError::Binary(format!(
            "identity {:?} does not match verified release identity {:?}",
            observed, expected
        )));
    }

    Ok(VerifiedReleaseBundle {
        root,
        identity: expected,
        _staging: staging,
    })
}

struct ResolvedRelease {
    release: ReleaseInfo,
    identity: ReleaseIdentity,
}

fn resolve_release_with<S: GitHubSource>(
    release: &ReleaseInfo,
    source: &S,
) -> Result<ResolvedRelease, BundleAcquisitionError> {
    validate_release_identity(release)?;
    let fresh_release = source.fetch_release_by_tag(release)?;
    validate_release_identity(&fresh_release)?;
    if fresh_release.version() != release.version()
        || fresh_release.channel() != release.channel()
        || fresh_release.tag_name() != release.tag_name()
    {
        return Err(BundleAcquisitionError::ReleaseMetadata(
            "fresh release-by-tag metadata disagrees with the selected release".to_string(),
        ));
    }
    select_release_assets(&fresh_release)?;
    let commit = source.resolve_tag_commit(fresh_release.tag_name())?;
    validate_commit_sha(&commit).map_err(BundleAcquisitionError::TagResolution)?;
    let identity = ReleaseIdentity::from_parts(
        fresh_release.tag_name(),
        fresh_release.version().clone(),
        fresh_release.channel(),
        RELEASE_TARGET,
        commit,
    );
    Ok(ResolvedRelease {
        release: fresh_release,
        identity,
    })
}

fn validate_release_identity(release: &ReleaseInfo) -> Result<(), BundleAcquisitionError> {
    let expected_tag = format!("v{}", release.version());
    if release.tag_name() != expected_tag {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "tag `{}` must be `{expected_tag}`",
            release.tag_name()
        )));
    }
    if !release.version().build.is_empty() {
        return Err(BundleAcquisitionError::ReleaseMetadata(
            "release version must not contain build metadata".to_string(),
        ));
    }
    let expected_channel = if release.version().pre.is_empty() {
        UpdateChannel::Stable
    } else {
        UpdateChannel::Prerelease
    };
    if release.channel() != expected_channel {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "version {} belongs to the {} channel, not {}",
            release.version(),
            expected_channel.as_str(),
            release.channel().as_str()
        )));
    }
    Ok(())
}

struct SelectedAssets<'a> {
    archive: &'a ReleaseAsset,
    checksum: &'a ReleaseAsset,
}

fn select_release_assets(
    release: &ReleaseInfo,
) -> Result<SelectedAssets<'_>, BundleAcquisitionError> {
    let archive_name = format!("lg-buddy-{}-{RELEASE_TARGET}.tar.gz", release.version());
    let archives: Vec<_> = release
        .assets()
        .iter()
        .filter(|asset| asset.name() == archive_name)
        .collect();
    let checksums: Vec<_> = release
        .assets()
        .iter()
        .filter(|asset| asset.name() == CHECKSUM_ASSET_NAME)
        .collect();

    let archive = exactly_one_asset(&archive_name, archives)?;
    let checksum = exactly_one_asset(CHECKSUM_ASSET_NAME, checksums)?;
    if archive.id() == checksum.id() {
        return Err(BundleAcquisitionError::ReleaseMetadata(
            "archive and checksum assets share the same GitHub asset ID".to_string(),
        ));
    }
    validate_asset_metadata(release, archive)?;
    validate_asset_metadata(release, checksum)?;
    if archive.size() > MAX_ARCHIVE_BYTES {
        return Err(BundleAcquisitionError::ResponseTooLarge {
            label: format!("release asset `{}`", archive.name()),
            max_bytes: MAX_ARCHIVE_BYTES,
        });
    }
    if checksum.size() > MAX_CHECKSUM_BYTES {
        return Err(BundleAcquisitionError::ResponseTooLarge {
            label: format!("release asset `{}`", checksum.name()),
            max_bytes: MAX_CHECKSUM_BYTES,
        });
    }

    Ok(SelectedAssets { archive, checksum })
}

fn exactly_one_asset<'a>(
    name: &str,
    assets: Vec<&'a ReleaseAsset>,
) -> Result<&'a ReleaseAsset, BundleAcquisitionError> {
    match assets.as_slice() {
        [asset] => Ok(*asset),
        [] => Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "release must contain exactly one `{name}` asset; found none"
        ))),
        _ => Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "release must contain exactly one `{name}` asset; found {}",
            assets.len()
        ))),
    }
}

fn validate_asset_metadata(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
) -> Result<(), BundleAcquisitionError> {
    if asset.state() != "uploaded" {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "asset `{}` is in GitHub state `{}` instead of `uploaded`",
            asset.name(),
            asset.state()
        )));
    }
    if asset.size() == 0 {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "asset `{}` has an empty size",
            asset.name()
        )));
    }
    required_github_digest(asset)?;

    let expected_api_url = format!(
        "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/assets/{}",
        asset.id()
    );
    if asset.api_url() != expected_api_url {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "asset `{}` API URL does not belong to the LG Buddy repository",
            asset.name()
        )));
    }

    let expected_download_url = expected_browser_download_url(release.tag_name(), asset.name());
    if asset.download_url() != expected_download_url {
        return Err(BundleAcquisitionError::ReleaseMetadata(format!(
            "asset `{}` download URL does not belong to the selected LG Buddy release",
            asset.name()
        )));
    }
    Ok(())
}

fn expected_browser_download_url(tag: &str, name: &str) -> String {
    format!(
        "https://github.com/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/download/{tag}/{name}"
    )
}

fn required_github_digest(asset: &ReleaseAsset) -> Result<String, BundleAcquisitionError> {
    let digest = asset.digest().ok_or_else(|| {
        BundleAcquisitionError::Digest(format!("asset `{}` has no GitHub digest", asset.name()))
    })?;
    let value = digest.strip_prefix("sha256:").ok_or_else(|| {
        BundleAcquisitionError::Digest(format!(
            "asset `{}` uses unsupported digest `{digest}`",
            asset.name()
        ))
    })?;
    validate_sha256(value).map_err(BundleAcquisitionError::Digest)?;
    Ok(value.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DownloadFacts {
    bytes: u64,
    sha256: String,
}

#[derive(Debug)]
struct DownloadedAsset {
    file: File,
    facts: DownloadFacts,
}

fn verify_download(
    asset: &ReleaseAsset,
    download: &DownloadFacts,
) -> Result<(), BundleAcquisitionError> {
    if download.bytes != asset.size() {
        return Err(BundleAcquisitionError::InterruptedAsset {
            name: asset.name().to_string(),
            expected: asset.size(),
            actual: download.bytes,
        });
    }
    let expected = required_github_digest(asset)?;
    if download.sha256 != expected {
        return Err(BundleAcquisitionError::Digest(format!(
            "asset `{}` computed `{}`, expected `{expected}`",
            asset.name(),
            download.sha256
        )));
    }
    Ok(())
}

fn published_archive_digest(
    checksum_file: &File,
    archive_name: &str,
) -> Result<String, BundleAcquisitionError> {
    let mut checksum_file = rewind_clone(checksum_file)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut checksum_file)
        .take(MAX_CHECKSUM_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CHECKSUM_BYTES {
        return Err(BundleAcquisitionError::ResponseTooLarge {
            label: CHECKSUM_ASSET_NAME.to_string(),
            max_bytes: MAX_CHECKSUM_BYTES,
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|err| {
        BundleAcquisitionError::Checksum(format!("checksum file is not valid UTF-8: {err}"))
    })?;
    let mut matches = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if line.len() < 67 {
            return Err(BundleAcquisitionError::Checksum(format!(
                "line {} is malformed",
                index + 1
            )));
        }
        let digest = line.get(..64).ok_or_else(|| {
            BundleAcquisitionError::Checksum(format!(
                "line {} splits a UTF-8 character in the digest field",
                index + 1
            ))
        })?;
        validate_sha256(digest).map_err(BundleAcquisitionError::Checksum)?;
        let separator = line.get(64..66).ok_or_else(|| {
            BundleAcquisitionError::Checksum(format!(
                "line {} splits a UTF-8 character at the checksum separator",
                index + 1
            ))
        })?;
        if separator != "  " && separator != " *" {
            return Err(BundleAcquisitionError::Checksum(format!(
                "line {} has an unsupported checksum separator",
                index + 1
            )));
        }
        let name = line.get(66..).ok_or_else(|| {
            BundleAcquisitionError::Checksum(format!(
                "line {} splits a UTF-8 character before the filename",
                index + 1
            ))
        })?;
        let normalized_name = name.strip_prefix("./").unwrap_or(name);
        if normalized_name == archive_name {
            matches.push(digest.to_string());
        }
    }
    match matches.as_slice() {
        [digest] => Ok(digest.clone()),
        [] => Err(BundleAcquisitionError::Checksum(format!(
            "no entry names `{archive_name}`"
        ))),
        _ => Err(BundleAcquisitionError::Checksum(format!(
            "multiple entries name `{archive_name}`"
        ))),
    }
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "`{value}` is not a lowercase hexadecimal SHA-256 digest"
        ));
    }
    Ok(())
}

trait GitHubSource {
    fn fetch_release_by_tag(
        &self,
        selected: &ReleaseInfo,
    ) -> Result<ReleaseInfo, BundleAcquisitionError>;

    fn resolve_tag_commit(&self, tag: &str) -> Result<String, BundleAcquisitionError>;

    fn download_asset(
        &self,
        asset: &ReleaseAsset,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<DownloadedAsset, BundleAcquisitionError>;
}

struct UreqGitHubSource {
    api_agent: ureq::Agent,
}

impl UreqGitHubSource {
    fn new() -> Self {
        Self {
            api_agent: github_agent(GITHUB_CONNECT_TIMEOUT, GITHUB_REQUEST_TIMEOUT),
        }
    }

    fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> Result<T, BundleAcquisitionError> {
        let response = match github_request(&self.api_agent, url, GITHUB_JSON_ACCEPT).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(status, response)) => {
                let body = read_response_text(response, url, MAX_API_ERROR_BYTES)?;
                return Err(BundleAcquisitionError::HttpStatus {
                    url: url.to_string(),
                    status,
                    body,
                });
            }
            Err(ureq::Error::Transport(err)) => {
                return Err(BundleAcquisitionError::Http {
                    url: url.to_string(),
                    message: err.to_string(),
                });
            }
        };
        if response.status() != 200 {
            return Err(BundleAcquisitionError::HttpStatus {
                url: url.to_string(),
                status: response.status(),
                body: String::new(),
            });
        }
        let body = read_response_bytes(response, url, MAX_API_BYTES)?;
        serde_json::from_slice(&body).map_err(|err| {
            BundleAcquisitionError::TagResolution(format!(
                "GitHub response from `{url}` was malformed: {err}"
            ))
        })
    }

    fn asset_response(
        &self,
        asset: &ReleaseAsset,
    ) -> Result<(ureq::Response, String), BundleAcquisitionError> {
        let deadline = Instant::now() + GITHUB_REQUEST_TIMEOUT;
        let url = format!(
            "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/assets/{}",
            asset.id()
        );
        let budget = remaining_request_budget(deadline)?;
        let agent = github_agent(budget.connect, budget.request);
        match github_request(&agent, &url, GITHUB_ASSET_ACCEPT)
            .set("Accept-Encoding", "identity")
            .call()
        {
            Ok(response) if response.status() == 200 => Ok((response, url)),
            Ok(response) if response.status() == 302 => {
                self.follow_asset_redirect(&url, response, deadline)
            }
            Ok(response) => Err(BundleAcquisitionError::HttpStatus {
                url,
                status: response.status(),
                body: String::new(),
            }),
            Err(ureq::Error::Status(302, response)) => {
                self.follow_asset_redirect(&url, response, deadline)
            }
            Err(ureq::Error::Status(status, response)) => {
                let body = read_response_text(response, &url, MAX_API_ERROR_BYTES)?;
                Err(BundleAcquisitionError::HttpStatus { url, status, body })
            }
            Err(ureq::Error::Transport(err)) => Err(BundleAcquisitionError::Http {
                url,
                message: err.to_string(),
            }),
        }
    }

    fn follow_asset_redirect(
        &self,
        original_url: &str,
        response: ureq::Response,
        deadline: Instant,
    ) -> Result<(ureq::Response, String), BundleAcquisitionError> {
        let location = response
            .header("Location")
            .ok_or_else(|| BundleAcquisitionError::Http {
                url: original_url.to_string(),
                message: "GitHub asset redirect omitted Location".to_string(),
            })?;
        validate_asset_redirect(location)?;
        let redirected_url = location.to_string();
        let safe_redirected_url = redact_url(&redirected_url);
        let budget = remaining_request_budget(deadline)?;
        let agent = github_agent(budget.connect, budget.request);
        match agent
            .get(&redirected_url)
            .set("Accept", GITHUB_ASSET_ACCEPT)
            .set("Accept-Encoding", "identity")
            .set(
                "User-Agent",
                concat!("lg-buddy/", env!("CARGO_PKG_VERSION")),
            )
            .call()
        {
            Ok(response) if response.status() == 200 => Ok((response, safe_redirected_url)),
            Ok(response) => Err(BundleAcquisitionError::HttpStatus {
                url: safe_redirected_url,
                status: response.status(),
                body: String::new(),
            }),
            Err(ureq::Error::Status(status, _)) => Err(BundleAcquisitionError::HttpStatus {
                url: safe_redirected_url,
                status,
                body: String::new(),
            }),
            Err(ureq::Error::Transport(_)) => Err(BundleAcquisitionError::Http {
                url: safe_redirected_url,
                message: "redirected asset request failed".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestBudget {
    connect: Duration,
    request: Duration,
}

fn remaining_request_budget(deadline: Instant) -> Result<RequestBudget, BundleAcquisitionError> {
    let request = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BundleAcquisitionError::Http {
            url: "GitHub release asset".to_string(),
            message: "request deadline expired".to_string(),
        })?;
    Ok(request_budget(request))
}

fn request_budget(request: Duration) -> RequestBudget {
    RequestBudget {
        connect: request.min(GITHUB_CONNECT_TIMEOUT),
        request,
    }
}

fn github_agent(connect_timeout: Duration, request_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(connect_timeout)
        .timeout(request_timeout)
        .https_only(true)
        .try_proxy_from_env(false)
        .redirects(0)
        .redirect_auth_headers(ureq::RedirectAuthHeaders::Never)
        .build()
}

fn github_request(agent: &ureq::Agent, url: &str, accept: &str) -> ureq::Request {
    agent
        .get(url)
        .set("Accept", accept)
        .set(
            "User-Agent",
            concat!("lg-buddy/", env!("CARGO_PKG_VERSION")),
        )
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
}

fn redact_url(value: &str) -> String {
    match Url::parse(value) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "GitHub release asset redirect".to_string(),
    }
}

impl GitHubSource for UreqGitHubSource {
    fn fetch_release_by_tag(
        &self,
        selected: &ReleaseInfo,
    ) -> Result<ReleaseInfo, BundleAcquisitionError> {
        let tag = selected.tag_name();
        if !valid_github_tag(tag) {
            return Err(BundleAcquisitionError::ReleaseMetadata(format!(
                "tag `{tag}` cannot be used in a GitHub release request"
            )));
        }
        let url = format!(
            "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/tags/{tag}"
        );
        let release: FreshGitHubRelease = self.get_json(&url)?;
        release_info_from_github_response(release, tag)
    }

    fn resolve_tag_commit(&self, tag: &str) -> Result<String, BundleAcquisitionError> {
        if !valid_github_tag(tag) {
            return Err(BundleAcquisitionError::TagResolution(format!(
                "tag `{tag}` cannot be used in a GitHub reference request"
            )));
        }
        let reference_url = format!(
            "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/git/ref/tags/{tag}"
        );
        let reference: GitReferenceResponse = self.get_json(&reference_url)?;
        peel_tag_object(reference.object, |sha| {
            let tag_url = format!(
                "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/git/tags/{sha}"
            );
            Ok(self.get_json::<GitTagResponse>(&tag_url)?.object)
        })
    }

    fn download_asset(
        &self,
        asset: &ReleaseAsset,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<DownloadedAsset, BundleAcquisitionError> {
        let (response, response_url) = self.asset_response(asset)?;
        if let Some(length) = response.header("Content-Length") {
            let length = length
                .parse::<u64>()
                .map_err(|_| BundleAcquisitionError::Http {
                    url: response_url.clone(),
                    message: "asset response has an invalid Content-Length".to_string(),
                })?;
            if length > max_bytes {
                return Err(BundleAcquisitionError::ResponseTooLarge {
                    label: format!("release asset `{}`", asset.name()),
                    max_bytes,
                });
            }
            if length != asset.size() {
                return Err(BundleAcquisitionError::InterruptedAsset {
                    name: asset.name().to_string(),
                    expected: asset.size(),
                    actual: length,
                });
            }
        }
        stream_response_to_file(response, &response_url, destination, max_bytes)
    }
}

fn valid_github_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Debug, Deserialize)]
struct FreshGitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<FreshGitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct FreshGitHubAsset {
    id: u64,
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    url: String,
    browser_download_url: String,
}

fn release_info_from_github_response(
    release: FreshGitHubRelease,
    expected_tag: &str,
) -> Result<ReleaseInfo, BundleAcquisitionError> {
    if release.draft || release.tag_name != expected_tag {
        return Err(BundleAcquisitionError::ReleaseMetadata(
            "fresh GitHub release metadata is draft or has the wrong tag".to_string(),
        ));
    }
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|err| {
        BundleAcquisitionError::ReleaseMetadata(format!(
            "fresh GitHub release tag is not a version: {err}"
        ))
    })?;
    let channel = if release.prerelease {
        UpdateChannel::Prerelease
    } else {
        UpdateChannel::Stable
    };
    Ok(ReleaseInfo::from_github(
        version,
        channel,
        release.html_url,
        release.tag_name,
        release
            .assets
            .into_iter()
            .map(|asset| {
                ReleaseAsset::from_github(
                    asset.id,
                    asset.name,
                    asset.state,
                    asset.size,
                    asset.digest,
                    asset.url,
                    asset.browser_download_url,
                )
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct GitReferenceResponse {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitTagResponse {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

fn peel_tag_object<F>(
    mut object: GitObject,
    mut fetch_tag: F,
) -> Result<String, BundleAcquisitionError>
where
    F: FnMut(&str) -> Result<GitObject, BundleAcquisitionError>,
{
    let mut visited = HashSet::new();
    for depth in 0..=MAX_TAG_DEPTH {
        validate_commit_sha(&object.sha).map_err(BundleAcquisitionError::TagResolution)?;
        match object.kind.as_str() {
            "commit" => return Ok(object.sha),
            "tag" if depth < MAX_TAG_DEPTH => {
                if !visited.insert(object.sha.clone()) {
                    return Err(BundleAcquisitionError::TagResolution(
                        "annotated tag chain contains a cycle".to_string(),
                    ));
                }
                object = fetch_tag(&object.sha)?;
            }
            "tag" => {
                return Err(BundleAcquisitionError::TagResolution(format!(
                    "annotated tag chain exceeds depth {MAX_TAG_DEPTH}"
                )));
            }
            other => {
                return Err(BundleAcquisitionError::TagResolution(format!(
                    "tag resolves to unsupported Git object type `{other}`"
                )));
            }
        }
    }
    Err(BundleAcquisitionError::TagResolution(
        "tag resolution did not reach a commit".to_string(),
    ))
}

fn validate_commit_sha(sha: &str) -> Result<(), String> {
    if sha.len() != 40
        || !sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("`{sha}` is not a full lowercase commit SHA"));
    }
    Ok(())
}

fn validate_asset_redirect(location: &str) -> Result<(), BundleAcquisitionError> {
    let url = Url::parse(location).map_err(|err| BundleAcquisitionError::Http {
        url: "GitHub release asset redirect".to_string(),
        message: format!("GitHub asset redirect is not a valid URL: {err}"),
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some(GITHUB_RELEASE_ASSET_HOST)
        || url.port_or_known_default() != Some(443)
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/github-production-release-asset/")
    {
        return Err(BundleAcquisitionError::Http {
            url: redact_url(location),
            message: "GitHub asset redirect target is not allowed".to_string(),
        });
    }
    Ok(())
}

fn read_response_text(
    response: ureq::Response,
    url: &str,
    max_bytes: u64,
) -> Result<String, BundleAcquisitionError> {
    let bytes = read_response_bytes(response, url, max_bytes)?;
    String::from_utf8(bytes).map_err(|err| BundleAcquisitionError::Http {
        url: url.to_string(),
        message: format!("response is not valid UTF-8: {err}"),
    })
}

fn read_response_bytes(
    response: ureq::Response,
    url: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, BundleAcquisitionError> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| BundleAcquisitionError::Http {
            url: url.to_string(),
            message: err.to_string(),
        })?;
    if bytes.len() as u64 > max_bytes {
        return Err(BundleAcquisitionError::ResponseTooLarge {
            label: format!("response from `{url}`"),
            max_bytes,
        });
    }
    Ok(bytes)
}

fn stream_response_to_file(
    response: ureq::Response,
    url: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<DownloadedAsset, BundleAcquisitionError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(destination)?;
    let mut reader = response.into_reader();
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| BundleAcquisitionError::Http {
                url: url.to_string(),
                message: err.to_string(),
            })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > max_bytes {
            return Err(BundleAcquisitionError::ResponseTooLarge {
                label: format!("response from `{url}`"),
                max_bytes,
            });
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(DownloadedAsset {
        file,
        facts: DownloadFacts {
            bytes: total,
            sha256: format!("{:x}", hasher.finalize()),
        },
    })
}

fn pin_download_file(path: &Path, file: &File) -> Result<(), BundleAcquisitionError> {
    let descriptor = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    let expected_uid = unsafe { libc::geteuid() };
    if !descriptor.file_type().is_file()
        || !named.file_type().is_file()
        || descriptor.uid() != expected_uid
        || descriptor.nlink() != 1
        || descriptor.mode() & 0o077 != 0
        || descriptor.dev() != named.dev()
        || descriptor.ino() != named.ino()
    {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "download `{}` is not the expected private regular file",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    if file.metadata()?.nlink() != 0 {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "download `{}` remained linked after pinning",
            path.display()
        )));
    }
    Ok(())
}

fn rewind_clone(file: &File) -> Result<File, BundleAcquisitionError> {
    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(0))?;
    Ok(clone)
}

#[derive(Debug)]
struct StagingDirectory {
    path: PathBuf,
    _lock: File,
    device: u64,
    inode: u64,
}

impl StagingDirectory {
    fn create(cache_root: &Path) -> Result<Self, BundleAcquisitionError> {
        ensure_private_directory(cache_root)?;
        let lock = open_lock_file(&cache_root.join(LOCK_FILE_NAME))?;
        let lock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if lock_result != 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                return Err(BundleAcquisitionError::ConcurrentAcquisition);
            }
            return Err(BundleAcquisitionError::Io(err));
        }

        for _ in 0..100 {
            let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = cache_root.join(format!("staging-{}-{sequence}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    if let Err(err) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    {
                        let _ = fs::remove_dir(&path);
                        return Err(BundleAcquisitionError::Io(err));
                    }
                    let metadata = match fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        Err(err) => {
                            let _ = fs::remove_dir(&path);
                            return Err(BundleAcquisitionError::Io(err));
                        }
                    };
                    if !metadata.file_type().is_dir()
                        || metadata.uid() != unsafe { libc::geteuid() }
                        || metadata.mode() & 0o077 != 0
                    {
                        let _ = fs::remove_dir(&path);
                        return Err(BundleAcquisitionError::UnsafeStaging(format!(
                            "new staging directory `{}` is not private",
                            path.display()
                        )));
                    }
                    return Ok(Self {
                        path,
                        _lock: lock,
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(BundleAcquisitionError::Io(err)),
            }
        }
        Err(BundleAcquisitionError::UnsafeStaging(
            "could not create a unique staging directory".to_string(),
        ))
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let expected_uid = unsafe { libc::geteuid() };
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_dir()
                && metadata.uid() == expected_uid
                && metadata.mode() & 0o077 == 0
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), BundleAcquisitionError> {
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "`{}` is not an absolute normalized path",
            path.display()
        )));
    }

    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => validate_staging_ancestor(&current, &metadata)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
                validate_staging_ancestor(&current, &fs::symlink_metadata(&current)?)?;
            }
            Err(err) => return Err(BundleAcquisitionError::Io(err)),
        }
    }

    let metadata = fs::symlink_metadata(path)?;
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "`{}` is owned by UID {}, expected {}",
            path.display(),
            metadata.uid(),
            expected_uid
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "`{}` is accessible to group or other users",
            path.display()
        )));
    }
    Ok(())
}

fn validate_staging_ancestor(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), BundleAcquisitionError> {
    if !metadata.file_type().is_dir() {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "`{}` is not a real directory",
            path.display()
        )));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != 0 && metadata.uid() != expected_uid {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "ancestor `{}` is owned by untrusted UID {}",
            path.display(),
            metadata.uid()
        )));
    }
    let writable_by_others = metadata.mode() & 0o022 != 0;
    let sticky = metadata.mode() & libc::S_ISVTX != 0;
    if writable_by_others && !sticky {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "ancestor `{}` is writable by other users without sticky protection",
            path.display()
        )));
    }
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File, BundleAcquisitionError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    let expected_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.uid() != expected_uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(BundleAcquisitionError::UnsafeStaging(format!(
            "lock file `{}` is not a private, singly linked regular file",
            path.display()
        )));
    }
    Ok(file)
}

#[derive(Debug)]
struct ArchivePlan {
    root_name: String,
    entries: HashMap<String, ArchiveEntryFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArchiveEntryFact {
    entry_type: EntryType,
    size: u64,
    mode: u32,
}

fn inspect_archive(
    archive_file: &File,
    archive_name: &str,
    expected: &ReleaseIdentity,
) -> Result<ArchivePlan, BundleAcquisitionError> {
    let expected_root = format!("lg-buddy-{}-{RELEASE_TARGET}", expected.version());
    let expected_archive = format!("{expected_root}.tar.gz");
    if archive_name != expected_archive {
        return Err(BundleAcquisitionError::Archive(format!(
            "archive name `{archive_name}` must be `{expected_archive}`"
        )));
    }
    let archive_metadata = archive_file.metadata()?;
    if !archive_metadata.file_type().is_file() || archive_metadata.nlink() > 1 {
        return Err(BundleAcquisitionError::Archive(
            "staged archive is not a singly linked regular file".to_string(),
        ));
    }
    if archive_metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(BundleAcquisitionError::ResponseTooLarge {
            label: format!("release archive `{archive_name}`"),
            max_bytes: MAX_ARCHIVE_BYTES,
        });
    }
    preflight_raw_archive(archive_file)?;
    let file = rewind_clone(archive_file)?;
    let mut archive = Archive::new(MultiGzDecoder::new(file));
    let mut validated_entries = HashMap::new();
    let mut expanded_bytes = 0_u64;
    let mut regular_files = 0_usize;
    let mut manifest = None;
    let entries = archive.entries().map_err(archive_error)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )));
        }
        let mut entry = entry.map_err(archive_error)?;
        let entry_type = entry.header().entry_type();
        let path = normalize_archive_path(entry.path_bytes().as_ref(), entry_type)?;
        if validated_entries.contains_key(&path) {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive contains duplicate path `{path}`"
            )));
        }
        validate_archive_layout(&path, &expected_root, entry_type)?;
        if entry.link_name_bytes().is_some() {
            return Err(BundleAcquisitionError::Archive(format!(
                "entry `{path}` contains link metadata"
            )));
        }
        let mode = entry.header().mode().map_err(archive_error)?;
        if mode & 0o7022 != 0 {
            return Err(BundleAcquisitionError::Archive(format!(
                "entry `{path}` has unsafe mode {mode:o}"
            )));
        }
        let size = entry.size();
        if entry_type == EntryType::Directory && size != 0 {
            return Err(BundleAcquisitionError::Archive(format!(
                "directory `{path}` has a nonzero payload"
            )));
        }
        if entry_type == EntryType::Regular {
            regular_files += 1;
            if regular_files > MAX_ARCHIVE_FILES {
                return Err(BundleAcquisitionError::Archive(format!(
                    "archive contains more than {MAX_ARCHIVE_FILES} regular files"
                )));
            }
        }
        validated_entries.insert(
            path.clone(),
            ArchiveEntryFact {
                entry_type,
                size,
                mode,
            },
        );
        if size > MAX_ARCHIVE_FILE_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "entry `{path}` exceeds the {MAX_ARCHIVE_FILE_BYTES}-byte file limit"
            )));
        }
        expanded_bytes = expanded_bytes.checked_add(size).ok_or_else(|| {
            BundleAcquisitionError::Archive("expanded archive size overflowed".to_string())
        })?;
        if expanded_bytes > MAX_ARCHIVE_EXPANDED_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "expanded archive exceeds the {MAX_ARCHIVE_EXPANDED_BYTES}-byte limit"
            )));
        }

        if path == format!("{expected_root}/{MANIFEST_NAME}") {
            if size > MAX_MANIFEST_BYTES {
                return Err(BundleAcquisitionError::Manifest(format!(
                    "manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"
                )));
            }
            let mut contents = Vec::new();
            entry
                .by_ref()
                .take(MAX_MANIFEST_BYTES + 1)
                .read_to_end(&mut contents)
                .map_err(|err| BundleAcquisitionError::Archive(err.to_string()))?;
            if contents.len() as u64 != size {
                return Err(BundleAcquisitionError::Archive(format!(
                    "entry `{path}` ended before its declared size"
                )));
            }
            manifest = Some(parse_manifest(&contents)?);
        } else if entry_type == EntryType::Regular {
            let copied = io::copy(&mut entry, &mut io::sink())
                .map_err(|err| BundleAcquisitionError::Archive(err.to_string()))?;
            if copied != size {
                return Err(BundleAcquisitionError::Archive(format!(
                    "entry `{path}` ended after {copied} bytes; header declares {size}"
                )));
            }
        }
    }

    reject_archive_trailing_data(&mut archive.into_inner())?;

    validate_required_layout(&validated_entries, &expected_root)?;
    let manifest = manifest.ok_or_else(|| {
        BundleAcquisitionError::Manifest(format!("archive does not contain `{MANIFEST_NAME}`"))
    })?;
    if &manifest != expected {
        return Err(BundleAcquisitionError::Manifest(format!(
            "identity {:?} does not match selected release identity {:?}",
            manifest, expected
        )));
    }
    Ok(ArchivePlan {
        root_name: expected_root,
        entries: validated_entries,
    })
}

fn preflight_raw_archive(archive_file: &File) -> Result<(), BundleAcquisitionError> {
    let file = rewind_clone(archive_file)?;
    let mut archive = Archive::new(MultiGzDecoder::new(file));
    let mut payload_bytes = 0_u64;
    let entries = archive.entries().map_err(archive_error)?.raw(true);
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive contains more than {MAX_ARCHIVE_ENTRIES} raw entries"
            )));
        }
        let mut entry = entry.map_err(archive_error)?;
        let entry_type = entry.header().entry_type();
        let size = entry.size();
        if is_archive_metadata_entry(entry_type) && size > MAX_ARCHIVE_METADATA_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive metadata entry exceeds the {MAX_ARCHIVE_METADATA_BYTES}-byte limit"
            )));
        }
        if !is_archive_metadata_entry(entry_type) && size > MAX_ARCHIVE_FILE_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "raw archive entry exceeds the {MAX_ARCHIVE_FILE_BYTES}-byte file limit"
            )));
        }
        payload_bytes = payload_bytes.checked_add(size).ok_or_else(|| {
            BundleAcquisitionError::Archive("raw archive payload size overflowed".to_string())
        })?;
        if payload_bytes > MAX_ARCHIVE_EXPANDED_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "raw archive payload exceeds the {MAX_ARCHIVE_EXPANDED_BYTES}-byte limit"
            )));
        }
        let copied = io::copy(&mut entry, &mut io::sink())
            .map_err(|err| BundleAcquisitionError::Archive(err.to_string()))?;
        if copied != size {
            return Err(BundleAcquisitionError::Archive(format!(
                "raw archive entry ended after {copied} bytes; header declares {size}"
            )));
        }
    }
    Ok(())
}

fn is_archive_metadata_entry(entry_type: EntryType) -> bool {
    entry_type.is_gnu_longname()
        || entry_type.is_gnu_longlink()
        || entry_type.is_pax_local_extensions()
        || entry_type.is_pax_global_extensions()
}

fn normalize_archive_path(
    bytes: &[u8],
    entry_type: EntryType,
) -> Result<String, BundleAcquisitionError> {
    if bytes.len() > MAX_ARCHIVE_PATH_BYTES {
        return Err(BundleAcquisitionError::Archive(format!(
            "archive path exceeds the {MAX_ARCHIVE_PATH_BYTES}-byte limit"
        )));
    }
    if bytes.contains(&0) {
        return Err(BundleAcquisitionError::Archive(
            "archive path contains a NUL byte".to_string(),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        BundleAcquisitionError::Archive("archive path is not valid UTF-8".to_string())
    })?;
    if text.ends_with('/') && entry_type != EntryType::Directory {
        return Err(BundleAcquisitionError::Archive(format!(
            "non-directory archive path `{text}` has a trailing slash"
        )));
    }
    let text = text.strip_suffix('/').unwrap_or(text);
    if text.is_empty()
        || text.starts_with('/')
        || text.contains('\\')
        || text
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(BundleAcquisitionError::Archive(format!(
            "archive path `{text}` is not a normalized relative path"
        )));
    }
    Ok(text.to_string())
}

fn validate_archive_layout(
    path: &str,
    root: &str,
    entry_type: EntryType,
) -> Result<(), BundleAcquisitionError> {
    if entry_type != EntryType::Regular && entry_type != EntryType::Directory {
        return Err(BundleAcquisitionError::Archive(format!(
            "entry `{path}` has unsupported type {:?}",
            entry_type
        )));
    }
    if path != root && !path.starts_with(&format!("{root}/")) {
        return Err(BundleAcquisitionError::Archive(format!(
            "entry `{path}` is outside expected root `{root}`"
        )));
    }
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .trim_start_matches('/');
    if relative.is_empty() {
        if entry_type != EntryType::Directory {
            return Err(BundleAcquisitionError::Archive(
                "bundle root must be a directory".to_string(),
            ));
        }
        return Ok(());
    }

    let allowed_file = required_relative_files().contains(&relative)
        || (relative.starts_with("docs/") && entry_type == EntryType::Regular);
    let allowed_directory = entry_type == EntryType::Directory
        && (matches!(relative, "bin" | "docs" | "systemd") || relative.starts_with("docs/"));
    if !allowed_file && !allowed_directory {
        return Err(BundleAcquisitionError::Archive(format!(
            "entry `{path}` is not part of the release-bundle layout"
        )));
    }
    if allowed_file && entry_type != EntryType::Regular {
        return Err(BundleAcquisitionError::Archive(format!(
            "required file `{path}` is not a regular file"
        )));
    }
    Ok(())
}

fn required_relative_files() -> &'static [&'static str] {
    &[
        "lg-buddy",
        "install.sh",
        "configure.sh",
        "uninstall.sh",
        "bin/LG_Buddy_Common",
        "LG_Buddy_Brightness.desktop",
        "README.md",
        "LICENSE",
        MANIFEST_NAME,
        "systemd/LG_Buddy.service",
        "systemd/LG_Buddy_lifecycle.service",
        "systemd/LG_Buddy_screen.service",
        "systemd/LG_Buddy_update_check.service",
        "systemd/LG_Buddy_update_check.timer",
        "systemd/lg_buddy.conf",
    ]
}

fn validate_required_layout(
    entries: &HashMap<String, ArchiveEntryFact>,
    root: &str,
) -> Result<(), BundleAcquisitionError> {
    for relative in required_relative_files() {
        let path = format!("{root}/{relative}");
        if !entries.contains_key(&path) {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive is missing required file `{path}`"
            )));
        }
    }
    for relative in ["", "bin", "docs", "systemd"] {
        let path = if relative.is_empty() {
            root.to_string()
        } else {
            format!("{root}/{relative}")
        };
        if !entries.contains_key(&path) {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive is missing required directory `{path}`"
            )));
        }
    }
    Ok(())
}

fn extract_archive(
    archive_file: &File,
    staging_root: &Path,
    plan: &ArchivePlan,
) -> Result<PathBuf, BundleAcquisitionError> {
    let file = rewind_clone(archive_file)?;
    let mut archive = Archive::new(MultiGzDecoder::new(file));
    let mut seen = HashSet::new();
    for entry in archive.entries().map_err(archive_error)? {
        let mut entry = entry.map_err(archive_error)?;
        let entry_type = entry.header().entry_type();
        let path = normalize_archive_path(entry.path_bytes().as_ref(), entry_type)?;
        if !seen.insert(path.clone()) {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive changed after validation: duplicate path `{path}`"
            )));
        }
        let mode = entry.header().mode().map_err(archive_error)?;
        let observed = ArchiveEntryFact {
            entry_type,
            size: entry.size(),
            mode,
        };
        if plan.entries.get(&path) != Some(&observed) || entry.link_name_bytes().is_some() {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive changed after validation at `{path}`"
            )));
        }
        let destination = staging_root.join(&path);
        if entry_type == EntryType::Directory {
            fs::create_dir_all(&destination)?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))?;
            continue;
        }
        let parent = destination.parent().ok_or_else(|| {
            BundleAcquisitionError::Archive(format!("entry `{path}` has no parent"))
        })?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let relative = path
            .strip_prefix(&format!("{}/", plan.root_name))
            .unwrap_or(&path);
        let mode = if executable_relative_files().contains(&relative) {
            0o700
        } else {
            0o600
        };
        let mut destination_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&destination)?;
        let expected_size = entry.size();
        let copied = io::copy(&mut entry, &mut destination_file)
            .map_err(|err| BundleAcquisitionError::Archive(err.to_string()))?;
        if copied != expected_size {
            return Err(BundleAcquisitionError::Archive(format!(
                "entry `{path}` extracted {copied} bytes; header declares {expected_size}"
            )));
        }
        destination_file.flush()?;
    }
    if seen.len() != plan.entries.len() {
        return Err(BundleAcquisitionError::Archive(
            "archive changed after validation: entries are missing".to_string(),
        ));
    }
    reject_archive_trailing_data(&mut archive.into_inner())?;
    Ok(staging_root.join(&plan.root_name))
}

fn reject_archive_trailing_data<R: Read>(reader: &mut R) -> Result<(), BundleAcquisitionError> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| BundleAcquisitionError::Archive(err.to_string()))?;
        if read == 0 {
            return Ok(());
        }
        total = total.saturating_add(read as u64);
        if total > MAX_ARCHIVE_TRAILING_BYTES {
            return Err(BundleAcquisitionError::Archive(format!(
                "archive has more than {MAX_ARCHIVE_TRAILING_BYTES} bytes after its end marker"
            )));
        }
        if buffer[..read].iter().any(|byte| *byte != 0) {
            return Err(BundleAcquisitionError::Archive(
                "archive contains data after its end marker".to_string(),
            ));
        }
    }
}

fn executable_relative_files() -> &'static [&'static str] {
    &[
        "lg-buddy",
        "install.sh",
        "configure.sh",
        "uninstall.sh",
        "bin/LG_Buddy_Common",
    ]
}

fn archive_error(err: impl fmt::Display) -> BundleAcquisitionError {
    BundleAcquisitionError::Archive(err.to_string())
}

#[derive(Debug)]
struct RawManifest {
    schema_version: Option<u64>,
    critical: Option<Vec<String>>,
    release_tag: Option<String>,
    version: Option<String>,
    channel: Option<String>,
    target: Option<String>,
    commit: Option<String>,
}

impl<'de> Deserialize<'de> for RawManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ManifestVisitor;

        impl<'de> Visitor<'de> for ManifestVisitor {
            type Value = RawManifest;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a release manifest object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut seen = HashSet::new();
                let mut manifest = RawManifest {
                    schema_version: None,
                    critical: None,
                    release_tag: None,
                    version: None,
                    channel: None,
                    target: None,
                    commit: None,
                };
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!(
                            "duplicate manifest field `{key}`"
                        )));
                    }
                    match key.as_str() {
                        "schema_version" => manifest.schema_version = Some(map.next_value()?),
                        "critical" => manifest.critical = Some(map.next_value()?),
                        "release_tag" => manifest.release_tag = Some(map.next_value()?),
                        "version" => manifest.version = Some(map.next_value()?),
                        "channel" => manifest.channel = Some(map.next_value()?),
                        "target" => manifest.target = Some(map.next_value()?),
                        "commit" => manifest.commit = Some(map.next_value()?),
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(manifest)
            }
        }

        deserializer.deserialize_map(ManifestVisitor)
    }
}

fn parse_manifest(contents: &[u8]) -> Result<ReleaseIdentity, BundleAcquisitionError> {
    if contents.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(BundleAcquisitionError::Manifest(format!(
            "manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"
        )));
    }
    let manifest: RawManifest = serde_json::from_slice(contents)
        .map_err(|err| BundleAcquisitionError::Manifest(err.to_string()))?;
    if manifest.schema_version != Some(1) {
        return Err(BundleAcquisitionError::Manifest(format!(
            "unsupported schema version {:?}",
            manifest.schema_version
        )));
    }
    let critical = manifest.critical.ok_or_else(|| {
        BundleAcquisitionError::Manifest("manifest has no critical field list".to_string())
    })?;
    let expected_critical = ["release_tag", "version", "channel", "target", "commit"];
    let mut seen = HashSet::new();
    for field in &critical {
        if !seen.insert(field.as_str()) {
            return Err(BundleAcquisitionError::Manifest(format!(
                "critical field `{field}` is duplicated"
            )));
        }
        if !expected_critical.contains(&field.as_str()) {
            return Err(BundleAcquisitionError::Manifest(format!(
                "unknown critical field `{field}`"
            )));
        }
    }
    for field in expected_critical {
        if !seen.contains(field) {
            return Err(BundleAcquisitionError::Manifest(format!(
                "identity field `{field}` is not marked critical"
            )));
        }
    }

    let release_tag = required_manifest_string(manifest.release_tag, "release_tag")?;
    let version_text = required_manifest_string(manifest.version, "version")?;
    let version = Version::parse(&version_text).map_err(|err| {
        BundleAcquisitionError::Manifest(format!("invalid version `{version_text}`: {err}"))
    })?;
    if !version.build.is_empty() {
        return Err(BundleAcquisitionError::Manifest(
            "version must not contain build metadata".to_string(),
        ));
    }
    if release_tag != format!("v{version}") {
        return Err(BundleAcquisitionError::Manifest(
            "release_tag must be exactly v followed by version".to_string(),
        ));
    }
    let channel_text = required_manifest_string(manifest.channel, "channel")?;
    let channel = match channel_text.as_str() {
        "stable" if version.pre.is_empty() => UpdateChannel::Stable,
        "prerelease" if !version.pre.is_empty() => UpdateChannel::Prerelease,
        "stable" | "prerelease" => {
            return Err(BundleAcquisitionError::Manifest(format!(
                "channel `{channel_text}` disagrees with version `{version}`"
            )));
        }
        _ => {
            return Err(BundleAcquisitionError::Manifest(format!(
                "unsupported channel `{channel_text}`"
            )));
        }
    };
    let target = required_manifest_string(manifest.target, "target")?;
    if !valid_target(&target) {
        return Err(BundleAcquisitionError::Manifest(format!(
            "invalid target `{target}`"
        )));
    }
    let commit = required_manifest_string(manifest.commit, "commit")?;
    validate_commit_sha(&commit).map_err(BundleAcquisitionError::Manifest)?;
    Ok(ReleaseIdentity {
        release_tag,
        version,
        channel,
        target,
        commit,
    })
}

fn required_manifest_string(
    value: Option<String>,
    field: &str,
) -> Result<String, BundleAcquisitionError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        BundleAcquisitionError::Manifest(format!("missing or invalid field `{field}`"))
    })
}

fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && target.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'_' | b'.' | b'-'))
        })
}

trait BinaryIdentityReader {
    fn read_identity(
        &self,
        binary: &Path,
        target: &str,
        release_tag: &str,
    ) -> Result<ReleaseIdentity, BundleAcquisitionError>;
}

struct EmbeddedBinaryIdentityReader;

impl BinaryIdentityReader for EmbeddedBinaryIdentityReader {
    fn read_identity(
        &self,
        binary: &Path,
        target: &str,
        release_tag: &str,
    ) -> Result<ReleaseIdentity, BundleAcquisitionError> {
        read_embedded_binary_identity(binary, target, release_tag)
    }
}

const EMBEDDED_IDENTITY_PREFIX: &[u8] = b"LG_BUDDY_RELEASE_IDENTITY_V1\0";
const EMBEDDED_IDENTITY_SUFFIX: &[u8] = b"\0LG_BUDDY_RELEASE_IDENTITY_END\0";
const EMBEDDED_IDENTITY_SECTION: &[u8] = b".lg_buddy.identity";

fn read_embedded_binary_identity(
    binary: &Path,
    target: &str,
    release_tag: &str,
) -> Result<ReleaseIdentity, BundleAcquisitionError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(binary)
        .map_err(|err| BundleAcquisitionError::Binary(err.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|err| BundleAcquisitionError::Binary(err.to_string()))?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.len() > MAX_ARCHIVE_FILE_BYTES
    {
        return Err(BundleAcquisitionError::Binary(
            "bundled binary is not a safe regular file".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARCHIVE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| BundleAcquisitionError::Binary(err.to_string()))?;
    if bytes.len() as u64 > MAX_ARCHIVE_FILE_BYTES {
        return Err(BundleAcquisitionError::Binary(
            "bundled binary exceeds the file-size limit".to_string(),
        ));
    }
    if bytes.get(..4) != Some(b"\x7fELF")
        || bytes.get(4) != Some(&2)
        || bytes.get(5) != Some(&1)
        || bytes.get(18..20) != Some(62_u16.to_le_bytes().as_slice())
    {
        return Err(BundleAcquisitionError::Binary(
            "bundled binary is not an x86-64 little-endian ELF file".to_string(),
        ));
    }

    let record = embedded_identity_section(&bytes)?;
    if !record.starts_with(EMBEDDED_IDENTITY_PREFIX) || !record.ends_with(EMBEDDED_IDENTITY_SUFFIX)
    {
        return Err(BundleAcquisitionError::Binary(
            "embedded identity section has an invalid envelope".to_string(),
        ));
    }
    let payload =
        &record[EMBEDDED_IDENTITY_PREFIX.len()..record.len() - EMBEDDED_IDENTITY_SUFFIX.len()];
    let identity =
        parse_manifest(payload).map_err(|err| BundleAcquisitionError::Binary(err.to_string()))?;
    if identity.target() != target || identity.release_tag() != release_tag {
        return Err(BundleAcquisitionError::Binary(format!(
            "embedded identity target/tag do not match `{target}` and `{release_tag}`"
        )));
    }
    Ok(identity)
}

fn embedded_identity_section(bytes: &[u8]) -> Result<&[u8], BundleAcquisitionError> {
    let section_offset = elf_u64(bytes, 40)?;
    let section_entry_size = elf_u16(bytes, 58)? as u64;
    let section_count = elf_u16(bytes, 60)? as u64;
    let names_index = elf_u16(bytes, 62)? as u64;
    if section_entry_size < 64
        || section_count == 0
        || section_count > 4_096
        || names_index >= section_count
    {
        return Err(BundleAcquisitionError::Binary(
            "binary has an invalid ELF section table".to_string(),
        ));
    }
    section_offset
        .checked_add(
            section_entry_size
                .checked_mul(section_count)
                .ok_or_else(|| {
                    BundleAcquisitionError::Binary("ELF section table size overflowed".to_string())
                })?,
        )
        .filter(|end| *end <= bytes.len() as u64)
        .ok_or_else(|| {
            BundleAcquisitionError::Binary("ELF section table extends beyond the file".to_string())
        })?;

    let (_, names_type, names_offset, names_size) =
        elf_section_header(bytes, section_offset, section_entry_size, names_index)?;
    if names_type != 3 {
        return Err(BundleAcquisitionError::Binary(
            "ELF section-name table has the wrong type".to_string(),
        ));
    }
    let names = checked_elf_slice(bytes, names_offset, names_size, "section-name table")?;
    let mut identity = None;
    for index in 0..section_count {
        let (name_offset, section_type, offset, size) =
            elf_section_header(bytes, section_offset, section_entry_size, index)?;
        let name_offset = name_offset as usize;
        let Some(name_tail) = names.get(name_offset..) else {
            return Err(BundleAcquisitionError::Binary(
                "ELF section name points outside the name table".to_string(),
            ));
        };
        let name_end = name_tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| {
                BundleAcquisitionError::Binary("ELF section name is not terminated".to_string())
            })?;
        if &name_tail[..name_end] != EMBEDDED_IDENTITY_SECTION {
            continue;
        }
        if section_type != 1 || size > MAX_MANIFEST_BYTES + 256 {
            return Err(BundleAcquisitionError::Binary(
                "embedded identity ELF section has an invalid type or size".to_string(),
            ));
        }
        let record = checked_elf_slice(bytes, offset, size, "embedded identity section")?;
        if identity.replace(record).is_some() {
            return Err(BundleAcquisitionError::Binary(
                "binary has multiple embedded identity sections".to_string(),
            ));
        }
    }
    identity.ok_or_else(|| {
        BundleAcquisitionError::Binary("binary has no embedded identity section".to_string())
    })
}

fn elf_section_header(
    bytes: &[u8],
    table_offset: u64,
    entry_size: u64,
    index: u64,
) -> Result<(u32, u32, u64, u64), BundleAcquisitionError> {
    let offset = table_offset
        .checked_add(entry_size.checked_mul(index).ok_or_else(|| {
            BundleAcquisitionError::Binary("ELF section offset overflowed".to_string())
        })?)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            BundleAcquisitionError::Binary("ELF section offset is invalid".to_string())
        })?;
    Ok((
        elf_u32(bytes, offset)?,
        elf_u32(bytes, offset + 4)?,
        elf_u64(bytes, offset + 24)?,
        elf_u64(bytes, offset + 32)?,
    ))
}

fn checked_elf_slice<'a>(
    bytes: &'a [u8],
    offset: u64,
    size: u64,
    label: &str,
) -> Result<&'a [u8], BundleAcquisitionError> {
    let start = usize::try_from(offset)
        .map_err(|_| BundleAcquisitionError::Binary(format!("ELF {label} offset is invalid")))?;
    let end = offset
        .checked_add(size)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| {
            BundleAcquisitionError::Binary(format!("ELF {label} extends beyond the file"))
        })?;
    bytes
        .get(start..end)
        .ok_or_else(|| BundleAcquisitionError::Binary(format!("ELF {label} range is invalid")))
}

fn elf_u16(bytes: &[u8], offset: usize) -> Result<u16, BundleAcquisitionError> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| BundleAcquisitionError::Binary("ELF header is truncated".to_string()))
}

fn elf_u32(bytes: &[u8], offset: usize) -> Result<u32, BundleAcquisitionError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| {
            BundleAcquisitionError::Binary("ELF section header is truncated".to_string())
        })
}

fn elf_u64(bytes: &[u8], offset: usize) -> Result<u64, BundleAcquisitionError> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| BundleAcquisitionError::Binary("ELF header is truncated".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::collections::HashMap;
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tar::{Builder, Header};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-release-bundle-{label}-{}-{}",
            process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn stable_identity() -> ReleaseIdentity {
        ReleaseIdentity {
            release_tag: "v1.4.0".to_string(),
            version: Version::parse("1.4.0").expect("version"),
            channel: UpdateChannel::Stable,
            target: "x86_64-unknown-linux-musl".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
        }
    }

    fn manifest_json(identity: &ReleaseIdentity) -> Vec<u8> {
        format!(
            "{{\"schema_version\":1,\"critical\":[\"release_tag\",\"version\",\"channel\",\"target\",\"commit\"],\"release_tag\":\"{}\",\"version\":\"{}\",\"channel\":\"{}\",\"target\":\"{}\",\"commit\":\"{}\"}}\n",
            identity.release_tag,
            identity.version,
            identity.channel.as_str(),
            identity.target,
            identity.commit
        )
        .into_bytes()
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn release_asset(id: u64, name: &str, bytes: &[u8]) -> ReleaseAsset {
        let tag = "v1.4.0";
        ReleaseAsset::from_github(
            id,
            name.to_string(),
            "uploaded".to_string(),
            bytes.len() as u64,
            Some(format!("sha256:{}", sha256(bytes))),
            format!(
                "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/assets/{id}"
            ),
            expected_browser_download_url(tag, name),
        )
    }

    fn release_info(assets: Vec<ReleaseAsset>) -> ReleaseInfo {
        ReleaseInfo::from_github(
            Version::parse("1.4.0").expect("version"),
            UpdateChannel::Stable,
            "https://github.com/Staphylococcus/LG_Buddy/releases/tag/v1.4.0".to_string(),
            "v1.4.0".to_string(),
            assets,
        )
    }

    fn github_response_release_info(
        identity: &ReleaseIdentity,
        assets: &[(u64, &str, &[u8])],
    ) -> ReleaseInfo {
        let response = serde_json::json!({
            "tag_name": identity.release_tag,
            "html_url": format!(
                "https://github.com/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/tag/{}",
                identity.release_tag
            ),
            "draft": false,
            "prerelease": identity.channel == UpdateChannel::Prerelease,
            "assets": assets
                .iter()
                .map(|(id, name, bytes)| serde_json::json!({
                    "id": id,
                    "name": name,
                    "state": "uploaded",
                    "size": bytes.len(),
                    "digest": format!("sha256:{}", sha256(bytes)),
                    "url": format!(
                        "{GITHUB_API_ROOT}/repos/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/assets/{id}"
                    ),
                    "browser_download_url": expected_browser_download_url(
                        &identity.release_tag,
                        name
                    ),
                }))
                .collect::<Vec<_>>(),
        });
        release_info_from_github_response(
            serde_json::from_value(response).expect("GitHub response fixture"),
            &identity.release_tag,
        )
        .expect("valid GitHub response fixture")
    }

    #[derive(Clone)]
    enum FixtureEntry {
        Directory(String, u32),
        File(String, Vec<u8>, u32),
        Symlink(String, String),
        Special(String, u8),
        RawPath(String),
    }

    fn valid_fixture_entries(identity: &ReleaseIdentity) -> Vec<FixtureEntry> {
        let root = format!("lg-buddy-{}-{RELEASE_TARGET}", identity.version);
        let mut entries = vec![
            FixtureEntry::Directory(root.clone(), 0o755),
            FixtureEntry::Directory(format!("{root}/bin"), 0o755),
            FixtureEntry::Directory(format!("{root}/docs"), 0o755),
            FixtureEntry::Directory(format!("{root}/systemd"), 0o755),
        ];
        for relative in required_relative_files() {
            let contents = if *relative == MANIFEST_NAME {
                manifest_json(identity)
            } else if *relative == "lg-buddy" {
                embedded_identity_binary(identity)
            } else {
                format!("fixture for {relative}\n").into_bytes()
            };
            let mode = if executable_relative_files().contains(relative) {
                0o755
            } else {
                0o644
            };
            entries.push(FixtureEntry::File(
                format!("{root}/{relative}"),
                contents,
                mode,
            ));
        }
        entries.push(FixtureEntry::File(
            format!("{root}/docs/testing.md"),
            b"documentation\n".to_vec(),
            0o644,
        ));
        entries.push(FixtureEntry::File(
            format!("{root}/docs/{}.md", "long-name-".repeat(20)),
            b"long-path documentation\n".to_vec(),
            0o644,
        ));
        entries
    }

    fn write_fixture_archive(
        directory: &Path,
        identity: &ReleaseIdentity,
        entries: &[FixtureEntry],
    ) -> PathBuf {
        let archive_name = format!("lg-buddy-{}-{RELEASE_TARGET}.tar.gz", identity.version);
        let path = directory.join(archive_name);
        let file = File::create(&path).expect("create archive");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);
        for fixture in entries {
            match fixture {
                FixtureEntry::Directory(path, mode) => {
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Directory);
                    header.set_mode(*mode);
                    header.set_size(0);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, format!("{path}/"), io::empty())
                        .expect("append directory");
                }
                FixtureEntry::File(path, contents, mode) => {
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Regular);
                    header.set_mode(*mode);
                    header.set_size(contents.len() as u64);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, Cursor::new(contents))
                        .expect("append file");
                }
                FixtureEntry::Symlink(path, target) => {
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    header.set_link_name(target).expect("link target");
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, io::empty())
                        .expect("append symlink");
                }
                FixtureEntry::Special(path, kind) => {
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::new(*kind));
                    header.set_mode(0o600);
                    header.set_size(0);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, io::empty())
                        .expect("append special entry");
                }
                FixtureEntry::RawPath(path) => {
                    assert!(path.len() <= 100);
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Regular);
                    header.set_mode(0o600);
                    header.set_size(0);
                    header.as_mut_bytes()[..100].fill(0);
                    header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
                    header.set_cksum();
                    builder
                        .append(&header, io::empty())
                        .expect("append raw-path entry");
                }
            }
        }
        let encoder = builder.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip");
        path
    }

    fn write_raw_extension_archive(
        directory: &Path,
        identity: &ReleaseIdentity,
        entry_type: EntryType,
        payload_size: u64,
    ) -> PathBuf {
        let archive_name = format!("lg-buddy-{}-{RELEASE_TARGET}.tar.gz", identity.version);
        let path = directory.join(archive_name);
        let mut encoder = GzEncoder::new(
            File::create(&path).expect("create raw extension archive"),
            Compression::default(),
        );
        let mut header = Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_mode(0o600);
        header.set_size(payload_size);
        header.set_cksum();
        encoder
            .write_all(header.as_bytes())
            .expect("write extension header");
        io::copy(&mut io::repeat(b'x').take(payload_size), &mut encoder)
            .expect("write extension payload");
        let padding = (512 - payload_size % 512) % 512;
        io::copy(&mut io::repeat(0).take(padding), &mut encoder).expect("write extension padding");
        encoder
            .write_all(&[0_u8; 1024])
            .expect("write tar end markers");
        encoder.finish().expect("finish raw extension gzip");
        path
    }

    fn read_file(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read fixture")
    }

    fn write_embedded_identity_binary(directory: &Path, identity: &ReleaseIdentity) -> PathBuf {
        let path = directory.join("lg-buddy");
        fs::write(&path, embedded_identity_binary(identity))
            .expect("write embedded identity fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make fixture executable");
        path
    }

    fn embedded_identity_binary(identity: &ReleaseIdentity) -> Vec<u8> {
        let mut bytes = vec![0_u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&64_u16.to_le_bytes());
        bytes[60..62].copy_from_slice(&3_u16.to_le_bytes());
        bytes[62..64].copy_from_slice(&1_u16.to_le_bytes());

        let names = b"\0.shstrtab\0.lg_buddy.identity\0";
        let names_offset = bytes.len() as u64;
        bytes.extend_from_slice(names);
        let identity_offset = bytes.len() as u64;
        let mut record = Vec::new();
        record.extend_from_slice(EMBEDDED_IDENTITY_PREFIX);
        record.extend_from_slice(&manifest_json(identity));
        record.extend_from_slice(EMBEDDED_IDENTITY_SUFFIX);
        bytes.extend_from_slice(&record);
        while !bytes.len().is_multiple_of(8) {
            bytes.push(0);
        }
        let section_offset = bytes.len() as u64;
        bytes[40..48].copy_from_slice(&section_offset.to_le_bytes());
        bytes.resize(bytes.len() + 3 * 64, 0);
        let names_header = section_offset as usize + 64;
        bytes[names_header..names_header + 4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[names_header + 4..names_header + 8].copy_from_slice(&3_u32.to_le_bytes());
        bytes[names_header + 24..names_header + 32].copy_from_slice(&names_offset.to_le_bytes());
        bytes[names_header + 32..names_header + 40]
            .copy_from_slice(&(names.len() as u64).to_le_bytes());
        let identity_header = section_offset as usize + 2 * 64;
        bytes[identity_header..identity_header + 4].copy_from_slice(&11_u32.to_le_bytes());
        bytes[identity_header + 4..identity_header + 8].copy_from_slice(&1_u32.to_le_bytes());
        bytes[identity_header + 24..identity_header + 32]
            .copy_from_slice(&identity_offset.to_le_bytes());
        bytes[identity_header + 32..identity_header + 40]
            .copy_from_slice(&(record.len() as u64).to_le_bytes());
        bytes
    }

    struct FakeSource {
        fresh_release: ReleaseInfo,
        payloads: HashMap<u64, Vec<u8>>,
        commit: String,
    }

    impl GitHubSource for FakeSource {
        fn fetch_release_by_tag(
            &self,
            _selected: &ReleaseInfo,
        ) -> Result<ReleaseInfo, BundleAcquisitionError> {
            Ok(self.fresh_release.clone())
        }

        fn resolve_tag_commit(&self, _tag: &str) -> Result<String, BundleAcquisitionError> {
            Ok(self.commit.clone())
        }

        fn download_asset(
            &self,
            asset: &ReleaseAsset,
            destination: &Path,
            max_bytes: u64,
        ) -> Result<DownloadedAsset, BundleAcquisitionError> {
            let bytes =
                self.payloads
                    .get(&asset.id())
                    .ok_or_else(|| BundleAcquisitionError::Http {
                        url: asset.api_url().to_string(),
                        message: "missing fake payload".to_string(),
                    })?;
            if bytes.len() as u64 > max_bytes {
                return Err(BundleAcquisitionError::ResponseTooLarge {
                    label: asset.name().to_string(),
                    max_bytes,
                });
            }
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(destination)?;
            file.write_all(bytes)?;
            file.flush()?;
            Ok(DownloadedAsset {
                file,
                facts: DownloadFacts {
                    bytes: bytes.len() as u64,
                    sha256: sha256(bytes),
                },
            })
        }
    }

    struct FakeBinaryIdentityReader {
        identity: ReleaseIdentity,
        calls: AtomicUsize,
    }

    impl FakeBinaryIdentityReader {
        fn new(identity: ReleaseIdentity) -> Self {
            Self {
                identity,
                calls: AtomicUsize::new(0),
            }
        }
    }

    struct UncalledSource;

    impl GitHubSource for UncalledSource {
        fn fetch_release_by_tag(
            &self,
            _selected: &ReleaseInfo,
        ) -> Result<ReleaseInfo, BundleAcquisitionError> {
            panic!("concurrent acquisition reached release metadata")
        }

        fn resolve_tag_commit(&self, _tag: &str) -> Result<String, BundleAcquisitionError> {
            panic!("concurrent acquisition reached tag resolution")
        }

        fn download_asset(
            &self,
            _asset: &ReleaseAsset,
            _destination: &Path,
            _max_bytes: u64,
        ) -> Result<DownloadedAsset, BundleAcquisitionError> {
            panic!("concurrent acquisition reached asset download")
        }
    }

    impl BinaryIdentityReader for FakeBinaryIdentityReader {
        fn read_identity(
            &self,
            _binary: &Path,
            _target: &str,
            _release_tag: &str,
        ) -> Result<ReleaseIdentity, BundleAcquisitionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.identity.clone())
        }
    }

    #[test]
    fn asset_redirect_policy_accepts_only_the_release_asset_host_and_path() {
        validate_asset_redirect(
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/file?sig=x",
        )
        .expect("allowed redirect");
        for rejected in [
            "http://release-assets.githubusercontent.com/github-production-release-asset/1/file",
            "https://example.com/github-production-release-asset/1/file",
            "https://release-assets.githubusercontent.com/unexpected/1/file",
            "https://user@release-assets.githubusercontent.com/github-production-release-asset/1/file",
            "https://release-assets.githubusercontent.com:444/github-production-release-asset/1/file",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/file#fragment",
        ] {
            assert!(validate_asset_redirect(rejected).is_err(), "{rejected}");
        }
        let error = validate_asset_redirect(
            "https://user:secret@evil.example/github-production-release-asset/1/file?token=signed",
        )
        .expect_err("userinfo redirect must fail")
        .to_string();
        assert!(!error.contains("user"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("signed"));
    }

    #[test]
    fn request_budget_never_allows_connect_to_outlive_the_remaining_deadline() {
        let full = request_budget(GITHUB_REQUEST_TIMEOUT);
        assert_eq!(full.connect, GITHUB_CONNECT_TIMEOUT);
        assert_eq!(full.request, GITHUB_REQUEST_TIMEOUT);

        let redirect_remaining = Duration::from_millis(25);
        let redirect = request_budget(redirect_remaining);
        assert_eq!(redirect.connect, redirect_remaining);
        assert_eq!(redirect.request, redirect_remaining);
    }

    #[test]
    fn release_identity_resolution_validates_metadata_without_downloading_assets() {
        let identity = stable_identity();
        let archive_name = format!("lg-buddy-1.4.0-{RELEASE_TARGET}.tar.gz");
        let source = FakeSource {
            fresh_release: release_info(vec![
                release_asset(1, &archive_name, b"archive"),
                release_asset(2, CHECKSUM_ASSET_NAME, b"checksum"),
            ]),
            payloads: HashMap::new(),
            commit: identity.commit.clone(),
        };

        let resolved = resolve_release_with(&release_info(Vec::new()), &source)
            .expect("metadata-only resolution should not download assets");

        assert_eq!(resolved.identity, identity);
    }

    #[test]
    fn sha256_requires_canonical_lowercase_hex() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());
        assert!(validate_sha256(&"z".repeat(64)).is_err());
    }

    #[test]
    fn checksum_requires_exactly_one_archive_entry() {
        let dir = temp_dir("checksum");
        let path = dir.join("sha256sums.txt");
        let digest = "a".repeat(64);
        fs::write(&path, format!("{digest}  bundle.tar.gz\n")).expect("write checksum");
        let file = File::open(&path).expect("open checksum");
        assert_eq!(
            published_archive_digest(&file, "bundle.tar.gz").expect("digest"),
            digest
        );
        fs::write(&path, format!("{digest}  ./bundle.tar.gz\n"))
            .expect("write workflow-style checksum");
        let file = File::open(&path).expect("open workflow checksum");
        assert_eq!(
            published_archive_digest(&file, "bundle.tar.gz").expect("workflow digest"),
            digest
        );
        fs::write(
            &path,
            format!("{digest}  bundle.tar.gz\n{digest} *bundle.tar.gz\n"),
        )
        .expect("write duplicate checksum");
        let file = File::open(&path).expect("open duplicate checksum");
        assert!(matches!(
            published_archive_digest(&file, "bundle.tar.gz"),
            Err(BundleAcquisitionError::Checksum(_))
        ));
        for malformed in [
            format!("{} bundle.tar.gz\n", "a".repeat(64)),
            format!("{}  other.tar.gz\n", "a".repeat(64)),
            format!("{}é  bundle.tar.gz\n", "a".repeat(63)),
            "not-a-checksum\n".to_string(),
        ] {
            fs::write(&path, malformed).expect("write malformed checksum");
            let file = File::open(&path).expect("open malformed checksum");
            assert!(published_archive_digest(&file, "bundle.tar.gz").is_err());
        }
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn pinned_download_descriptor_ignores_later_path_replacement() {
        let dir = temp_dir("pinned-download");
        let path = dir.join("asset");
        let mut original = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("create original");
        original.write_all(b"verified").expect("write original");
        pin_download_file(&path, &original).expect("pin original");
        fs::write(&path, b"replacement").expect("create replacement");

        let mut pinned = rewind_clone(&original).expect("rewind pinned file");
        let mut contents = Vec::new();
        pinned.read_to_end(&mut contents).expect("read pinned file");
        assert_eq!(contents, b"verified");
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn manifest_rejects_duplicate_fields_and_identity_mismatches() {
        let identity = stable_identity();
        let valid = format!(
            "{{\"schema_version\":1,\"critical\":[\"release_tag\",\"version\",\"channel\",\"target\",\"commit\"],\"release_tag\":\"{}\",\"version\":\"{}\",\"channel\":\"{}\",\"target\":\"{}\",\"commit\":\"{}\"}}",
            identity.release_tag,
            identity.version,
            identity.channel.as_str(),
            identity.target,
            identity.commit
        );
        assert_eq!(
            parse_manifest(valid.as_bytes()).expect("manifest"),
            identity
        );
        let duplicate = valid.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        );
        assert!(parse_manifest(duplicate.as_bytes()).is_err());
        let wrong_channel = valid.replace("\"channel\":\"stable\"", "\"channel\":\"prerelease\"");
        assert!(parse_manifest(wrong_channel.as_bytes()).is_err());
        for invalid in [
            valid.replace("\"release_tag\":\"v1.4.0\"", "\"release_tag\":\"v1.4.1\""),
            valid.replace("\"version\":\"1.4.0\"", "\"version\":\"1.4.0+build\""),
            valid.replace(
                "\"target\":\"x86_64-unknown-linux-musl\"",
                "\"target\":\"/unsafe\"",
            ),
            valid.replace("0123456789abcdef0123456789abcdef01234567", "not-a-commit"),
            valid.replace("\"commit\"]", "\"unknown\"]"),
            valid.replace("\"commit\"]", "\"target\"]"),
        ] {
            assert!(parse_manifest(invalid.as_bytes()).is_err(), "{invalid}");
        }
    }

    #[test]
    fn embedded_binary_identity_reader_requires_exact_elf_identity() {
        let dir = temp_dir("embedded-binary-identity");
        let identity = stable_identity();
        let binary = write_embedded_identity_binary(&dir, &identity);
        assert_eq!(
            read_embedded_binary_identity(&binary, RELEASE_TARGET, "v1.4.0")
                .expect("embedded identity"),
            identity
        );
        assert!(read_embedded_binary_identity(&binary, RELEASE_TARGET, "v1.4.1").is_err());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn current_test_binary_exposes_exactly_one_identity_elf_section() {
        let binary = std::env::current_exe().expect("current test executable");
        let bytes = fs::read(binary).expect("read current test executable");
        let record = embedded_identity_section(&bytes).expect("embedded identity section");
        assert!(record.starts_with(EMBEDDED_IDENTITY_PREFIX));
        assert!(record.ends_with(EMBEDDED_IDENTITY_SUFFIX));
        let payload =
            &record[EMBEDDED_IDENTITY_PREFIX.len()..record.len() - EMBEDDED_IDENTITY_SUFFIX.len()];
        let manifest: serde_json::Value =
            serde_json::from_slice(payload).expect("embedded identity JSON");
        assert!(manifest["target"]
            .as_str()
            .is_some_and(|target| !target.is_empty()));
    }

    #[test]
    fn embedded_binary_identity_rejects_wrong_target_and_non_elf_files() {
        let dir = temp_dir("invalid-embedded-binary");
        let wrong_target = ReleaseIdentity {
            target: "x86_64-unknown-linux-gnu".to_string(),
            ..stable_identity()
        };
        let binary = write_embedded_identity_binary(&dir, &wrong_target);
        assert!(read_embedded_binary_identity(&binary, RELEASE_TARGET, "v1.4.0").is_err());

        fs::write(&binary, b"not an ELF executable").expect("replace with script-shaped file");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("fixture mode");
        assert!(read_embedded_binary_identity(&binary, RELEASE_TARGET, "v1.4.0").is_err());

        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn asset_selection_requires_one_uploaded_bounded_asset_of_each_exact_name() {
        let archive_name = format!("lg-buddy-1.4.0-{RELEASE_TARGET}.tar.gz");
        let archive = release_asset(1, &archive_name, b"archive");
        let checksum = release_asset(2, CHECKSUM_ASSET_NAME, b"checksum");
        let release = release_info(vec![archive.clone(), checksum.clone()]);
        let selected = select_release_assets(&release).expect("select assets");
        assert_eq!(selected.archive.id(), 1);
        assert_eq!(selected.checksum.id(), 2);

        assert!(select_release_assets(&release_info(vec![checksum.clone()])).is_err());
        assert!(select_release_assets(&release_info(vec![
            archive.clone(),
            archive.clone(),
            checksum.clone(),
        ]))
        .is_err());
        let wrong_target = release_asset(
            3,
            "lg-buddy-1.4.0-aarch64-unknown-linux-musl.tar.gz",
            b"archive",
        );
        assert!(
            select_release_assets(&release_info(vec![wrong_target, checksum.clone()])).is_err()
        );

        let pending = ReleaseAsset::from_github(
            1,
            archive_name.clone(),
            "new".to_string(),
            7,
            archive.digest().map(str::to_string),
            archive.api_url().to_string(),
            archive.download_url().to_string(),
        );
        assert!(select_release_assets(&release_info(vec![pending, checksum.clone()])).is_err());
        let oversized = ReleaseAsset::from_github(
            1,
            archive_name,
            "uploaded".to_string(),
            MAX_ARCHIVE_BYTES + 1,
            archive.digest().map(str::to_string),
            archive.api_url().to_string(),
            archive.download_url().to_string(),
        );
        assert!(matches!(
            select_release_assets(&release_info(vec![oversized, checksum])),
            Err(BundleAcquisitionError::ResponseTooLarge { .. })
        ));
    }

    #[test]
    fn asset_selection_rejects_missing_digest_and_repository_url_substitution() {
        let archive_name = format!("lg-buddy-1.4.0-{RELEASE_TARGET}.tar.gz");
        let archive = release_asset(1, &archive_name, b"archive");
        let checksum = release_asset(2, CHECKSUM_ASSET_NAME, b"checksum");
        let missing_digest = ReleaseAsset::from_github(
            archive.id(),
            archive.name().to_string(),
            "uploaded".to_string(),
            archive.size(),
            None,
            archive.api_url().to_string(),
            archive.download_url().to_string(),
        );
        assert!(matches!(
            select_release_assets(&release_info(vec![missing_digest, checksum.clone()])),
            Err(BundleAcquisitionError::Digest(_))
        ));
        let foreign_url = ReleaseAsset::from_github(
            archive.id(),
            archive.name().to_string(),
            "uploaded".to_string(),
            archive.size(),
            archive.digest().map(str::to_string),
            "https://api.github.com/repos/other/project/releases/assets/1".to_string(),
            archive.download_url().to_string(),
        );
        assert!(matches!(
            select_release_assets(&release_info(vec![foreign_url, checksum])),
            Err(BundleAcquisitionError::ReleaseMetadata(_))
        ));
    }

    #[test]
    fn tag_peeling_accepts_lightweight_and_bounded_annotated_tags() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            peel_tag_object(
                GitObject {
                    kind: "commit".to_string(),
                    sha: commit.to_string(),
                },
                |_| panic!("lightweight tag must not fetch a tag object")
            )
            .expect("lightweight tag"),
            commit
        );

        let annotated = "1111111111111111111111111111111111111111";
        assert_eq!(
            peel_tag_object(
                GitObject {
                    kind: "tag".to_string(),
                    sha: annotated.to_string(),
                },
                |sha| {
                    assert_eq!(sha, annotated);
                    Ok(GitObject {
                        kind: "commit".to_string(),
                        sha: commit.to_string(),
                    })
                }
            )
            .expect("annotated tag"),
            commit
        );
    }

    #[test]
    fn tag_peeling_rejects_cycles_depth_invalid_shas_and_non_commits() {
        let tag_sha = "1111111111111111111111111111111111111111";
        assert!(peel_tag_object(
            GitObject {
                kind: "tag".to_string(),
                sha: tag_sha.to_string(),
            },
            |_| Ok(GitObject {
                kind: "tag".to_string(),
                sha: tag_sha.to_string(),
            })
        )
        .is_err());

        let mut depth = 1_u64;
        assert!(peel_tag_object(
            GitObject {
                kind: "tag".to_string(),
                sha: format!("{depth:040x}"),
            },
            |_| {
                depth += 1;
                Ok(GitObject {
                    kind: "tag".to_string(),
                    sha: format!("{depth:040x}"),
                })
            }
        )
        .is_err());
        assert!(peel_tag_object(
            GitObject {
                kind: "commit".to_string(),
                sha: "short".to_string(),
            },
            |_| unreachable!()
        )
        .is_err());
        assert!(peel_tag_object(
            GitObject {
                kind: "tree".to_string(),
                sha: tag_sha.to_string(),
            },
            |_| unreachable!()
        )
        .is_err());
    }

    #[test]
    fn archive_path_normalization_rejects_aliases_and_escape_shapes() {
        assert_eq!(
            normalize_archive_path(b"root/file", EntryType::Regular).expect("normal path"),
            "root/file"
        );
        assert_eq!(
            normalize_archive_path(b"root/dir/", EntryType::Directory).expect("directory path"),
            "root/dir"
        );
        for rejected in [
            b"/root/file".as_slice(),
            b"root/../file",
            b"root/./file",
            b"root//file",
            b"root\\file",
            b"",
        ] {
            assert!(
                normalize_archive_path(rejected, EntryType::Regular).is_err(),
                "{rejected:?}"
            );
        }
        assert!(normalize_archive_path(b"root/file/", EntryType::Regular).is_err());
        assert!(normalize_archive_path(b"root/\0file", EntryType::Regular).is_err());
    }

    #[test]
    fn archive_layout_rejects_every_link_device_and_special_entry_type() {
        let root = "lg-buddy-1.4.0-x86_64-unknown-linux-musl";
        for kind in *b"123467SLK" {
            assert!(validate_archive_layout(
                &format!("{root}/docs/entry"),
                root,
                EntryType::new(kind),
            )
            .is_err());
        }
    }

    #[test]
    fn archive_trailing_data_check_is_strict_and_bounded() {
        reject_archive_trailing_data(&mut Cursor::new(vec![0_u8; 1024]))
            .expect("bounded zero padding");
        assert!(reject_archive_trailing_data(&mut Cursor::new(b"\0unexpected")).is_err());
        assert!(reject_archive_trailing_data(&mut Cursor::new(vec![
            0_u8;
            (MAX_ARCHIVE_TRAILING_BYTES + 1)
                as usize
        ]))
        .is_err());
    }

    #[test]
    fn valid_archive_is_fully_inspected_then_extracted_with_private_modes() {
        let dir = temp_dir("valid-archive");
        let identity = stable_identity();
        let archive = write_fixture_archive(&dir, &identity, &valid_fixture_entries(&identity));
        let name = archive
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        let archive_file = File::open(&archive).expect("open archive");
        let plan = inspect_archive(&archive_file, name, &identity).expect("inspect archive");
        let extraction = dir.join("extraction");
        fs::create_dir(&extraction).expect("create extraction root");
        let root = extract_archive(&archive_file, &extraction, &plan).expect("extract archive");
        assert_eq!(
            fs::symlink_metadata(root.join("lg-buddy"))
                .expect("binary metadata")
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(root.join("release-manifest.json"))
                .expect("manifest metadata")
                .mode()
                & 0o777,
            0o600
        );
        assert!(root
            .join("docs")
            .join(format!("{}.md", "long-name-".repeat(20)))
            .is_file());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn archive_rejects_duplicate_link_unsafe_mode_missing_and_unexpected_entries() {
        let identity = stable_identity();
        let root = format!("lg-buddy-{}-{RELEASE_TARGET}", identity.version);
        let cases = [
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.push(entries[4].clone());
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.push(FixtureEntry::Symlink(
                    format!("{root}/docs/link"),
                    "../../outside".to_string(),
                ));
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.push(FixtureEntry::Special(format!("{root}/docs/fifo"), b'6'));
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.push(FixtureEntry::RawPath("../outside".to_string()));
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                if let FixtureEntry::File(_, _, mode) = &mut entries[4] {
                    *mode = 0o666;
                }
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.retain(|entry| {
                    !matches!(entry, FixtureEntry::File(path, _, _) if path.ends_with("/README.md"))
                });
                entries
            },
            {
                let mut entries = valid_fixture_entries(&identity);
                entries.push(FixtureEntry::File(
                    "other-root/file".to_string(),
                    b"unexpected".to_vec(),
                    0o644,
                ));
                entries
            },
        ];
        for (index, entries) in cases.into_iter().enumerate() {
            let dir = temp_dir(&format!("invalid-archive-{index}"));
            let archive = write_fixture_archive(&dir, &identity, &entries);
            let name = archive
                .file_name()
                .and_then(|name| name.to_str())
                .expect("name");
            let archive_file = File::open(&archive).expect("open archive");
            assert!(
                inspect_archive(&archive_file, name, &identity).is_err(),
                "case {index}"
            );
            fs::remove_dir_all(dir).expect("remove test directory");
        }
    }

    #[test]
    fn archive_rejects_manifest_identity_mismatch_and_truncation() {
        let identity = stable_identity();
        let mut entries = valid_fixture_entries(&identity);
        for entry in &mut entries {
            if let FixtureEntry::File(path, contents, _) = entry {
                if path.ends_with(MANIFEST_NAME) {
                    *contents = manifest_json(&ReleaseIdentity {
                        commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                        ..identity.clone()
                    });
                }
            }
        }
        let dir = temp_dir("manifest-mismatch");
        let archive = write_fixture_archive(&dir, &identity, &entries);
        let name = archive
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        let archive_file = File::open(&archive).expect("open archive");
        assert!(matches!(
            inspect_archive(&archive_file, name, &identity),
            Err(BundleAcquisitionError::Manifest(_))
        ));

        let archive = write_fixture_archive(&dir, &identity, &valid_fixture_entries(&identity));
        let length = fs::metadata(&archive).expect("archive metadata").len();
        OpenOptions::new()
            .write(true)
            .open(&archive)
            .expect("open archive")
            .set_len(length / 2)
            .expect("truncate archive");
        let name = archive
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        let archive_file = File::open(&archive).expect("open truncated archive");
        assert!(inspect_archive(&archive_file, name, &identity).is_err());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn archive_rejects_a_trailing_gzip_member() {
        let identity = stable_identity();
        let dir = temp_dir("trailing-gzip-member");
        let archive = write_fixture_archive(&dir, &identity, &valid_fixture_entries(&identity));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(b"unexpected trailing member")
            .expect("gzip payload");
        let trailing = encoder.finish().expect("finish trailing gzip");
        OpenOptions::new()
            .append(true)
            .open(&archive)
            .expect("open archive for append")
            .write_all(&trailing)
            .expect("append trailing gzip member");
        let name = archive
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        let archive_file = File::open(&archive).expect("open archive");
        assert!(inspect_archive(&archive_file, name, &identity).is_err());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn archive_inspection_enforces_the_compressed_size_limit() {
        let identity = stable_identity();
        let dir = temp_dir("oversized-compressed-archive");
        let name = format!("lg-buddy-{}-{RELEASE_TARGET}.tar.gz", identity.version());
        let path = dir.join(&name);
        File::create(&path)
            .expect("create sparse archive")
            .set_len(MAX_ARCHIVE_BYTES + 1)
            .expect("size sparse archive");
        let file = File::open(&path).expect("open sparse archive");
        assert!(matches!(
            inspect_archive(&file, &name, &identity),
            Err(BundleAcquisitionError::ResponseTooLarge { .. })
        ));
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn raw_archive_preflight_rejects_oversized_hidden_extension_payloads() {
        let identity = stable_identity();
        for (label, entry_type) in [
            ("gnu-long-name", EntryType::new(b'L')),
            ("pax-local", EntryType::new(b'x')),
        ] {
            let dir = temp_dir(label);
            let archive = write_raw_extension_archive(
                &dir,
                &identity,
                entry_type,
                MAX_ARCHIVE_METADATA_BYTES + 1,
            );
            let file = File::open(&archive).expect("open extension archive");
            let name = archive
                .file_name()
                .and_then(|name| name.to_str())
                .expect("archive name");
            assert!(matches!(
                inspect_archive(&file, name, &identity),
                Err(BundleAcquisitionError::Archive(message))
                    if message.contains("archive metadata entry exceeds")
            ));
            fs::remove_dir_all(dir).expect("remove test directory");
        }
    }

    #[test]
    fn archive_inspection_rejects_an_oversized_declared_entry_before_reading_it() {
        let identity = stable_identity();
        let dir = temp_dir("oversized-archive-entry");
        let name = format!("lg-buddy-{}-{RELEASE_TARGET}.tar.gz", identity.version());
        let path = dir.join(&name);
        let mut header = Header::new_gnu();
        header
            .set_path(format!(
                "lg-buddy-{}-{RELEASE_TARGET}/docs/oversized",
                identity.version()
            ))
            .expect("entry path");
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(MAX_ARCHIVE_FILE_BYTES + 1);
        header.set_cksum();
        let mut encoder = GzEncoder::new(
            File::create(&path).expect("create archive"),
            Compression::default(),
        );
        encoder
            .write_all(header.as_bytes())
            .expect("write tar header");
        encoder
            .write_all(&[0_u8; 1024])
            .expect("write tar end markers");
        encoder.finish().expect("finish gzip");

        let file = File::open(&path).expect("open archive");
        assert!(inspect_archive(&file, &name, &identity).is_err());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn staging_lock_excludes_concurrent_attempts_and_cleanup_is_guard_owned() {
        let dir = temp_dir("staging-lock");
        let cache_root = dir.join("cache");
        let first = StagingDirectory::create(&cache_root).expect("first staging");
        let first_path = first.path.clone();
        assert!(matches!(
            StagingDirectory::create(&cache_root),
            Err(BundleAcquisitionError::ConcurrentAcquisition)
        ));
        assert!(first_path.is_dir());
        drop(first);
        assert!(!first_path.exists());
        assert!(cache_root.join(LOCK_FILE_NAME).is_file());
        let next = StagingDirectory::create(&cache_root).expect("next staging");
        drop(next);
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn staging_cleanup_does_not_remove_a_replacement_directory() {
        let dir = temp_dir("staging-replacement");
        let cache_root = dir.join("cache");
        let staging = StagingDirectory::create(&cache_root).expect("create staging");
        let original = staging.path.clone();
        let moved = cache_root.join("moved-original");
        fs::rename(&original, &moved).expect("move original staging");
        fs::create_dir(&original).expect("create replacement staging");
        fs::set_permissions(&original, fs::Permissions::from_mode(0o700))
            .expect("secure replacement");
        drop(staging);
        assert!(original.is_dir());
        assert!(moved.is_dir());
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn concurrent_acquisition_fails_before_any_source_operation() {
        let dir = temp_dir("concurrent-before-source");
        let cache_root = dir.join("cache");
        let active = StagingDirectory::create(&cache_root).expect("active staging");
        let active_path = active.path.clone();
        let binary_reader = FakeBinaryIdentityReader::new(stable_identity());
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &cache_root,
                &UncalledSource,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::ConcurrentAcquisition)
        ));
        assert!(active_path.is_dir());
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        drop(active);
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn staging_rejects_a_hard_linked_lock_file() {
        let dir = temp_dir("hard-linked-lock");
        let cache_root = dir.join("cache");
        let first = StagingDirectory::create(&cache_root).expect("create staging");
        drop(first);
        fs::hard_link(
            cache_root.join(LOCK_FILE_NAME),
            cache_root.join("lock-alias"),
        )
        .expect("hard link lock");
        assert!(matches!(
            StagingDirectory::create(&cache_root),
            Err(BundleAcquisitionError::UnsafeStaging(_))
        ));
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn staging_rejects_relative_symlinked_and_shared_writable_roots() {
        assert!(matches!(
            StagingDirectory::create(Path::new("relative/cache")),
            Err(BundleAcquisitionError::UnsafeStaging(_))
        ));
        let dir = temp_dir("unsafe-staging");
        let target = dir.join("target");
        fs::create_dir(&target).expect("create target");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        assert!(matches!(
            StagingDirectory::create(&link.join("cache")),
            Err(BundleAcquisitionError::UnsafeStaging(_))
        ));
        let shared = dir.join("shared");
        fs::create_dir(&shared).expect("create shared");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).expect("set shared mode");
        assert!(matches!(
            StagingDirectory::create(&shared.join("cache")),
            Err(BundleAcquisitionError::UnsafeStaging(_))
        ));
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn github_response_replay_returns_only_a_verified_owned_stage() {
        let identity = stable_identity();
        let fixture_dir = temp_dir("complete-acquisition");
        let archive_path =
            write_fixture_archive(&fixture_dir, &identity, &valid_fixture_entries(&identity));
        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("archive name")
            .to_string();
        let archive_bytes = read_file(&archive_path);
        let checksum_bytes = format!("{}  {archive_name}\n", sha256(&archive_bytes)).into_bytes();
        let fresh_release = github_response_release_info(
            &identity,
            &[
                (1, archive_name.as_str(), archive_bytes.as_slice()),
                (2, CHECKSUM_ASSET_NAME, checksum_bytes.as_slice()),
            ],
        );
        let source = FakeSource {
            fresh_release,
            payloads: HashMap::from([(1, archive_bytes), (2, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let cache_root = fixture_dir.join("cache");
        let verified = acquire_release_bundle_with(
            &release_info(Vec::new()),
            &cache_root,
            &source,
            &EmbeddedBinaryIdentityReader,
        )
        .expect("verified acquisition");
        assert_eq!(verified.identity(), &identity);
        assert!(verified.root().join("install.sh").is_file());
        let staging_root = verified
            .root()
            .parent()
            .expect("candidate root parent")
            .to_path_buf();
        assert!(staging_root.is_dir());
        drop(verified);
        assert!(!staging_root.exists());
        assert!(cache_root.join(LOCK_FILE_NAME).is_file());
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn acquisition_rejects_fresh_release_identity_disagreement_before_download() {
        let fixture_dir = temp_dir("fresh-release-disagreement");
        let source = FakeSource {
            fresh_release: ReleaseInfo::from_github(
                Version::parse("1.4.1").expect("version"),
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.4.1".to_string(),
                "v1.4.1".to_string(),
                Vec::new(),
            ),
            payloads: HashMap::new(),
            commit: stable_identity().commit,
        };
        let binary_reader = FakeBinaryIdentityReader::new(stable_identity());
        let cache_root = fixture_dir.join("cache");
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &cache_root,
                &source,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::ReleaseMetadata(_))
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        assert!(fs::read_dir(&cache_root)
            .expect("read cache root")
            .all(|entry| entry.expect("cache entry").file_name() == LOCK_FILE_NAME));
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn integrity_failure_cleans_staging_without_running_the_binary() {
        let identity = stable_identity();
        let fixture_dir = temp_dir("failed-acquisition");
        let archive_path =
            write_fixture_archive(&fixture_dir, &identity, &valid_fixture_entries(&identity));
        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("archive name")
            .to_string();
        let archive_bytes = read_file(&archive_path);
        let checksum_bytes = format!("{}  {archive_name}\n", "a".repeat(64)).into_bytes();
        let source = FakeSource {
            fresh_release: release_info(vec![
                release_asset(1, &archive_name, &archive_bytes),
                release_asset(2, CHECKSUM_ASSET_NAME, &checksum_bytes),
            ]),
            payloads: HashMap::from([(1, archive_bytes), (2, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let binary_reader = FakeBinaryIdentityReader::new(identity);
        let cache_root = fixture_dir.join("cache");
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &cache_root,
                &source,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::Checksum(_))
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        let entries: Vec<_> = fs::read_dir(&cache_root)
            .expect("read cache root")
            .map(|entry| entry.expect("cache entry").file_name())
            .collect();
        assert_eq!(entries, vec![LOCK_FILE_NAME]);
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn interrupted_asset_is_rejected_and_staging_is_removed() {
        let identity = stable_identity();
        let fixture_dir = temp_dir("interrupted-acquisition");
        let archive_name = format!("lg-buddy-1.4.0-{RELEASE_TARGET}.tar.gz");
        let declared_archive = b"complete archive";
        let checksum_bytes = format!("{}  {archive_name}\n", sha256(declared_archive)).into_bytes();
        let source = FakeSource {
            fresh_release: release_info(vec![
                release_asset(1, &archive_name, declared_archive),
                release_asset(2, CHECKSUM_ASSET_NAME, &checksum_bytes),
            ]),
            payloads: HashMap::from([(1, b"short".to_vec()), (2, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let binary_reader = FakeBinaryIdentityReader::new(identity);
        let cache_root = fixture_dir.join("cache");
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &cache_root,
                &source,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::InterruptedAsset { .. })
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        assert!(fs::read_dir(&cache_root)
            .expect("read cache root")
            .all(|entry| entry.expect("cache entry").file_name() == LOCK_FILE_NAME));
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn same_size_digest_mismatch_is_rejected_before_archive_or_binary_work() {
        let identity = stable_identity();
        let fixture_dir = temp_dir("digest-mismatch");
        let archive_name = format!("lg-buddy-1.4.0-{RELEASE_TARGET}.tar.gz");
        let declared_archive = b"expected";
        let checksum_bytes = format!("{}  {archive_name}\n", sha256(declared_archive)).into_bytes();
        let source = FakeSource {
            fresh_release: release_info(vec![
                release_asset(1, &archive_name, declared_archive),
                release_asset(2, CHECKSUM_ASSET_NAME, &checksum_bytes),
            ]),
            payloads: HashMap::from([(1, b"tampered".to_vec()), (2, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let binary_reader = FakeBinaryIdentityReader::new(identity);
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &fixture_dir.join("cache"),
                &source,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::Digest(_))
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn binary_identity_mismatch_removes_the_fully_extracted_stage() {
        let identity = stable_identity();
        let fixture_dir = temp_dir("binary-mismatch");
        let archive_path =
            write_fixture_archive(&fixture_dir, &identity, &valid_fixture_entries(&identity));
        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("archive name")
            .to_string();
        let archive_bytes = read_file(&archive_path);
        let checksum_bytes = format!("{}  {archive_name}\n", sha256(&archive_bytes)).into_bytes();
        let source = FakeSource {
            fresh_release: release_info(vec![
                release_asset(1, &archive_name, &archive_bytes),
                release_asset(2, CHECKSUM_ASSET_NAME, &checksum_bytes),
            ]),
            payloads: HashMap::from([(1, archive_bytes), (2, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let binary_reader = FakeBinaryIdentityReader::new(ReleaseIdentity {
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            ..identity
        });
        let cache_root = fixture_dir.join("cache");
        assert!(matches!(
            acquire_release_bundle_with(
                &release_info(Vec::new()),
                &cache_root,
                &source,
                &binary_reader,
            ),
            Err(BundleAcquisitionError::Binary(_))
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 1);
        assert!(fs::read_dir(&cache_root)
            .expect("read cache root")
            .all(|entry| entry.expect("cache entry").file_name() == LOCK_FILE_NAME));
        fs::remove_dir_all(fixture_dir).expect("remove test directory");
    }

    #[test]
    fn observed_historical_beta_response_is_rejected_at_the_manifest_boundary() {
        let identity = ReleaseIdentity {
            release_tag: "v1.4.0-beta.1".to_string(),
            version: Version::parse("1.4.0-beta.1").expect("version"),
            channel: UpdateChannel::Prerelease,
            target: RELEASE_TARGET.to_string(),
            commit: "12326a4acdfc0dccb532e389e8d4ae5edeb78c20".to_string(),
        };
        let dir = temp_dir("observed-historical-beta");
        let mut entries = valid_fixture_entries(&identity);
        entries.retain(|entry| {
            !matches!(entry, FixtureEntry::File(path, _, _) if path.ends_with(MANIFEST_NAME))
        });
        let archive_path = write_fixture_archive(&dir, &identity, &entries);
        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("archive name")
            .to_string();
        let archive_bytes = read_file(&archive_path);
        let checksum_bytes = format!("{}  ./{archive_name}\n", sha256(&archive_bytes)).into_bytes();
        let fresh_release = github_response_release_info(
            &identity,
            &[
                (536_289_792, archive_name.as_str(), archive_bytes.as_slice()),
                (536_289_793, CHECKSUM_ASSET_NAME, checksum_bytes.as_slice()),
            ],
        );
        let source = FakeSource {
            fresh_release,
            payloads: HashMap::from([(536_289_792, archive_bytes), (536_289_793, checksum_bytes)]),
            commit: identity.commit.clone(),
        };
        let selected = ReleaseInfo::from_github(
            identity.version.clone(),
            identity.channel,
            format!(
                "https://github.com/{REPOSITORY_OWNER}/{REPOSITORY_NAME}/releases/tag/{}",
                identity.release_tag
            ),
            identity.release_tag.clone(),
            Vec::new(),
        );
        let binary_reader = FakeBinaryIdentityReader::new(identity);
        let error =
            acquire_release_bundle_with(&selected, &dir.join("cache"), &source, &binary_reader)
                .expect_err("historical beta predates the manifest contract");
        assert!(matches!(
            error,
            BundleAcquisitionError::Archive(message) if message.contains(MANIFEST_NAME)
        ));
        assert_eq!(binary_reader.calls.load(Ordering::Relaxed), 0);
        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
