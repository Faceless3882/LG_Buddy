//! Profile-scoped platform access-token storage.

use crate::auth::SystemUser;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;

const TVS_DIR_NAME: &str = "tvs";
const PRIMARY_TV_PROFILE_NAME: &str = "primary";
const ACCESS_TOKEN_FILE_NAME: &str = "access-token.json";
const TEMP_FILE_ATTEMPTS: u8 = 100;

#[derive(Clone, PartialEq, Eq)]
pub struct PlatformAccessToken(String);

impl PlatformAccessToken {
    pub fn new(value: impl Into<String>) -> Result<Self, PlatformAccessTokenError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PlatformAccessTokenError::Empty);
        }

        Ok(Self(value))
    }

    pub fn as_secret_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlatformAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PlatformAccessToken")
            .field(&"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformAccessTokenError {
    Empty,
}

impl fmt::Display for PlatformAccessTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "platform access token cannot be empty"),
        }
    }
}

impl Error for PlatformAccessTokenError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformAccessTokenStoreOperation {
    CreateDirectory,
    InspectDirectory,
    OpenDirectory,
    SetPermissions,
    SetOwner,
    ReadToken,
    CreateTemporaryFile,
    WriteTemporaryFile,
    SyncTemporaryFile,
    ReplaceToken,
}

impl fmt::Display for PlatformAccessTokenStoreOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            Self::CreateDirectory => "create credential directory",
            Self::InspectDirectory => "inspect credential directory",
            Self::OpenDirectory => "open credential directory",
            Self::SetPermissions => "set credential permissions",
            Self::SetOwner => "set credential owner",
            Self::ReadToken => "read platform access token",
            Self::CreateTemporaryFile => "create temporary access-token file",
            Self::WriteTemporaryFile => "write temporary access-token file",
            Self::SyncTemporaryFile => "sync temporary access-token file",
            Self::ReplaceToken => "replace platform access-token file",
        };
        write!(f, "{description}")
    }
}

#[derive(Debug)]
pub enum PlatformAccessTokenStoreError {
    ConfigPathHasNoParent {
        path: PathBuf,
    },
    Io {
        operation: PlatformAccessTokenStoreOperation,
        path: PathBuf,
        source: io::Error,
    },
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidToken {
        path: PathBuf,
        source: PlatformAccessTokenError,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for PlatformAccessTokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigPathHasNoParent { path } => write!(
                f,
                "could not derive the platform access-token path because config path `{}` has no parent directory",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "could not {operation} at `{}`: {source}", path.display()),
            Self::InvalidJson { path, source } => write!(
                f,
                "platform access-token file `{}` is malformed: {source}",
                path.display()
            ),
            Self::InvalidToken { path, source } => write!(
                f,
                "platform access-token file `{}` is invalid: {source}",
                path.display()
            ),
            Self::Serialize(source) => {
                write!(f, "could not serialize platform access token: {source}")
            }
        }
    }
}

impl Error for PlatformAccessTokenStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            Self::InvalidToken { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::ConfigPathHasNoParent { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlatformAccessTokenStore {
    token_path: PathBuf,
    owner: SystemUser,
}

impl PlatformAccessTokenStore {
    pub fn for_primary_profile(
        config_path: &Path,
        owner: SystemUser,
    ) -> Result<Self, PlatformAccessTokenStoreError> {
        let config_dir = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| PlatformAccessTokenStoreError::ConfigPathHasNoParent {
                path: config_path.to_path_buf(),
            })?;

        Ok(Self {
            token_path: config_dir
                .join(TVS_DIR_NAME)
                .join(PRIMARY_TV_PROFILE_NAME)
                .join(ACCESS_TOKEN_FILE_NAME),
            owner,
        })
    }

    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    pub fn load(&self) -> Result<Option<PlatformAccessToken>, PlatformAccessTokenStoreError> {
        let contents = match read_token_file(&self.token_path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(store_io_error(
                    PlatformAccessTokenStoreOperation::ReadToken,
                    &self.token_path,
                    source,
                ))
            }
        };

        let stored: StoredPlatformAccessToken =
            serde_json::from_str(&contents).map_err(|source| {
                PlatformAccessTokenStoreError::InvalidJson {
                    path: self.token_path.clone(),
                    source,
                }
            })?;
        let token = PlatformAccessToken::new(stored.access_token).map_err(|source| {
            PlatformAccessTokenStoreError::InvalidToken {
                path: self.token_path.clone(),
                source,
            }
        })?;

        Ok(Some(token))
    }

    pub fn persist(
        &self,
        token: &PlatformAccessToken,
    ) -> Result<(), PlatformAccessTokenStoreError> {
        let mut contents = serde_json::to_vec_pretty(&StoredPlatformAccessToken {
            access_token: token.as_secret_str().to_string(),
        })
        .map_err(PlatformAccessTokenStoreError::Serialize)?;
        contents.push(b'\n');

        let profile_dir = self
            .token_path
            .parent()
            .expect("derived token path always has a profile directory");
        let tvs_dir = profile_dir
            .parent()
            .expect("derived profile path always has a TVs directory");

        ensure_private_directory(tvs_dir, &self.owner)?;
        ensure_private_directory(profile_dir, &self.owner)?;
        atomic_write_token(&self.token_path, &contents, &self.owner)
    }
}

#[derive(Deserialize, Serialize)]
struct StoredPlatformAccessToken {
    access_token: String,
}

fn read_token_file(path: &Path) -> io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);

    let mut contents = String::new();
    options.open(path)?.read_to_string(&mut contents)?;
    Ok(contents)
}

fn ensure_private_directory(
    path: &Path,
    owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(store_io_error(
                PlatformAccessTokenStoreOperation::CreateDirectory,
                path,
                source,
            ))
        }
    }

    let metadata = fs::symlink_metadata(path).map_err(|source| {
        store_io_error(
            PlatformAccessTokenStoreOperation::InspectDirectory,
            path,
            source,
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(store_io_error(
            PlatformAccessTokenStoreOperation::InspectDirectory,
            path,
            io::Error::new(
                io::ErrorKind::NotADirectory,
                "credential path is not a directory",
            ),
        ));
    }

    secure_directory(path, owner)
}

fn atomic_write_token(
    path: &Path,
    contents: &[u8],
    owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    let mut last_collision = None;

    for attempt in 0..TEMP_FILE_ATTEMPTS {
        let temp_path = token_temp_path(path, attempt);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }

        let mut file = match options.open(&temp_path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some((temp_path, source));
                continue;
            }
            Err(source) => {
                return Err(store_io_error(
                    PlatformAccessTokenStoreOperation::CreateTemporaryFile,
                    &temp_path,
                    source,
                ))
            }
        };

        let write_result = (|| {
            file.write_all(contents).map_err(|source| {
                store_io_error(
                    PlatformAccessTokenStoreOperation::WriteTemporaryFile,
                    &temp_path,
                    source,
                )
            })?;
            file.flush().map_err(|source| {
                store_io_error(
                    PlatformAccessTokenStoreOperation::WriteTemporaryFile,
                    &temp_path,
                    source,
                )
            })?;
            secure_file(&file, &temp_path, owner)?;
            file.sync_all().map_err(|source| {
                store_io_error(
                    PlatformAccessTokenStoreOperation::SyncTemporaryFile,
                    &temp_path,
                    source,
                )
            })?;
            drop(file);
            fs::rename(&temp_path, path).map_err(|source| {
                store_io_error(
                    PlatformAccessTokenStoreOperation::ReplaceToken,
                    path,
                    source,
                )
            })
        })();

        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        return Ok(());
    }

    let (path, source) = last_collision.unwrap_or_else(|| {
        (
            path.to_path_buf(),
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not create a unique temporary access-token file",
            ),
        )
    });
    Err(store_io_error(
        PlatformAccessTokenStoreOperation::CreateTemporaryFile,
        &path,
        source,
    ))
}

fn token_temp_path(path: &Path, attempt: u8) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("access-token");
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", process::id(), attempt))
}

fn store_io_error(
    operation: PlatformAccessTokenStoreOperation,
    path: &Path,
    source: io::Error,
) -> PlatformAccessTokenStoreError {
    PlatformAccessTokenStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn secure_directory(path: &Path, owner: &SystemUser) -> Result<(), PlatformAccessTokenStoreError> {
    let directory = File::open(path).map_err(|source| {
        store_io_error(
            PlatformAccessTokenStoreOperation::OpenDirectory,
            path,
            source,
        )
    })?;
    set_file_mode(&directory, path, 0o700)?;
    set_file_owner(&directory, path, owner)
}

#[cfg(not(unix))]
fn secure_directory(
    _path: &Path,
    _owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(
    file: &File,
    path: &Path,
    owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    set_file_mode(file, path, 0o600)?;
    set_file_owner(file, path, owner)
}

#[cfg(not(unix))]
fn secure_file(
    _file: &File,
    _path: &Path,
    _owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &File, path: &Path, mode: u32) -> Result<(), PlatformAccessTokenStoreError> {
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| {
            store_io_error(
                PlatformAccessTokenStoreOperation::SetPermissions,
                path,
                source,
            )
        })
}

#[cfg(unix)]
fn set_file_owner(
    file: &File,
    path: &Path,
    owner: &SystemUser,
) -> Result<(), PlatformAccessTokenStoreError> {
    let metadata = file.metadata().map_err(|source| {
        store_io_error(PlatformAccessTokenStoreOperation::SetOwner, path, source)
    })?;
    if metadata.uid() == owner.uid() && metadata.gid() == owner.gid() {
        return Ok(());
    }

    let result = unsafe { libc::fchown(file.as_raw_fd(), owner.uid(), owner.gid()) };
    if result == 0 {
        Ok(())
    } else {
        Err(store_io_error(
            PlatformAccessTokenStoreOperation::SetOwner,
            path,
            io::Error::last_os_error(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        token_temp_path, PlatformAccessToken, PlatformAccessTokenError, PlatformAccessTokenStore,
        PlatformAccessTokenStoreError, PlatformAccessTokenStoreOperation, TEMP_FILE_ATTEMPTS,
    };
    use crate::auth::SystemUser;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-web-os-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn config_path(&self) -> PathBuf {
            self.path().join("config.env")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn current_user(home: &Path) -> SystemUser {
        #[cfg(unix)]
        {
            SystemUser::new(
                "test-user",
                unsafe { libc::geteuid() },
                unsafe { libc::getegid() },
                home,
            )
        }

        #[cfg(not(unix))]
        {
            SystemUser::new("test-user", 0, 0, home)
        }
    }

    fn token(value: &str) -> PlatformAccessToken {
        PlatformAccessToken::new(value).expect("valid platform access token")
    }

    #[test]
    fn platform_access_token_rejects_empty_values_and_redacts_debug_output() {
        assert_eq!(
            PlatformAccessToken::new("  "),
            Err(PlatformAccessTokenError::Empty)
        );

        let token = token("secret-client-key");
        let debug = format!("{token:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(token.as_secret_str()));
    }

    #[test]
    fn primary_profile_token_path_is_derived_from_config_directory() {
        let owner = SystemUser::new("vas", 1000, 1000, "/home/vas");
        let store = PlatformAccessTokenStore::for_primary_profile(
            Path::new("/home/vas/.config/lg-buddy/config.env"),
            owner,
        )
        .expect("derive platform access-token store");

        assert_eq!(
            store.token_path(),
            Path::new("/home/vas/.config/lg-buddy/tvs/primary/access-token.json")
        );
        assert!(!store.token_path().to_string_lossy().contains("webos"));
    }

    #[test]
    fn primary_profile_token_path_requires_config_parent() {
        let owner = SystemUser::new("vas", 1000, 1000, "/home/vas");
        let error = PlatformAccessTokenStore::for_primary_profile(Path::new("config.env"), owner)
            .expect_err("bare config filename should not have a parent directory");

        assert!(matches!(
            error,
            PlatformAccessTokenStoreError::ConfigPathHasNoParent { .. }
        ));
    }

    #[test]
    fn missing_token_file_returns_none() {
        let dir = TestDir::new("missing-token");
        let store = PlatformAccessTokenStore::for_primary_profile(
            &dir.config_path(),
            current_user(dir.path()),
        )
        .expect("derive platform access-token store");

        assert_eq!(store.load().expect("load missing token"), None);
    }

    #[test]
    fn persist_creates_private_profile_path_and_round_trips_token() {
        let dir = TestDir::new("persist-token");
        let owner = current_user(dir.path());
        let store =
            PlatformAccessTokenStore::for_primary_profile(&dir.config_path(), owner.clone())
                .expect("derive platform access-token store");
        let expected = token("stored-client-key");

        store.persist(&expected).expect("persist access token");

        assert_eq!(store.load().expect("load persisted token"), Some(expected));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            let profile_dir = store.token_path().parent().expect("profile directory");
            let tvs_dir = profile_dir.parent().expect("TVs directory");
            for path in [tvs_dir, profile_dir] {
                let metadata = fs::metadata(path).expect("credential directory metadata");
                assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
                assert_eq!(metadata.uid(), owner.uid());
                assert_eq!(metadata.gid(), owner.gid());
            }

            let metadata = fs::metadata(store.token_path()).expect("token file metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), owner.uid());
            assert_eq!(metadata.gid(), owner.gid());
        }
    }

    #[test]
    fn malformed_json_and_empty_token_are_distinct_typed_errors() {
        let dir = TestDir::new("invalid-token");
        let store = PlatformAccessTokenStore::for_primary_profile(
            &dir.config_path(),
            current_user(dir.path()),
        )
        .expect("derive platform access-token store");
        fs::create_dir_all(store.token_path().parent().expect("profile directory"))
            .expect("create profile directory");

        fs::write(store.token_path(), "not-json\n").expect("write malformed token file");
        assert!(matches!(
            store.load().expect_err("malformed JSON should fail"),
            PlatformAccessTokenStoreError::InvalidJson { .. }
        ));

        fs::write(store.token_path(), r#"{"access_token":" "}"#).expect("write empty token file");
        assert!(matches!(
            store.load().expect_err("empty token should fail"),
            PlatformAccessTokenStoreError::InvalidToken {
                source: PlatformAccessTokenError::Empty,
                ..
            }
        ));
    }

    #[test]
    fn unreadable_token_path_is_a_typed_read_error() {
        let dir = TestDir::new("unreadable-token");
        let store = PlatformAccessTokenStore::for_primary_profile(
            &dir.config_path(),
            current_user(dir.path()),
        )
        .expect("derive platform access-token store");
        fs::create_dir_all(store.token_path()).expect("create directory at token path");

        assert!(matches!(
            store.load().expect_err("directory cannot be read as token"),
            PlatformAccessTokenStoreError::Io {
                operation: PlatformAccessTokenStoreOperation::ReadToken,
                ..
            }
        ));
    }

    #[test]
    fn temporary_file_exhaustion_does_not_replace_existing_token() {
        let dir = TestDir::new("atomic-failure");
        let store = PlatformAccessTokenStore::for_primary_profile(
            &dir.config_path(),
            current_user(dir.path()),
        )
        .expect("derive platform access-token store");
        let original = token("original-client-key");
        store.persist(&original).expect("persist original token");

        for attempt in 0..TEMP_FILE_ATTEMPTS {
            fs::write(token_temp_path(store.token_path(), attempt), [])
                .expect("occupy temporary token path");
        }

        let error = store
            .persist(&token("replacement-client-key"))
            .expect_err("occupied temporary paths should fail persistence");
        assert!(matches!(
            error,
            PlatformAccessTokenStoreError::Io {
                operation: PlatformAccessTokenStoreOperation::CreateTemporaryFile,
                ..
            }
        ));
        assert_eq!(store.load().expect("load original token"), Some(original));
    }

    #[cfg(unix)]
    #[test]
    fn persist_assigns_requested_owner_when_running_as_root() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }

        let dir = TestDir::new("root-owner");
        let owner = SystemUser::new("non-root-owner", 65534, 65534, "/nonexistent");
        let store =
            PlatformAccessTokenStore::for_primary_profile(&dir.config_path(), owner.clone())
                .expect("derive platform access-token store");

        store
            .persist(&token("root-written-client-key"))
            .expect("persist token for requested non-root owner");

        for path in [
            store
                .token_path()
                .parent()
                .expect("profile directory")
                .parent()
                .expect("TVs directory"),
            store.token_path().parent().expect("profile directory"),
            store.token_path(),
        ] {
            use std::os::unix::fs::MetadataExt;

            let metadata = fs::metadata(path).expect("credential path metadata");
            assert_eq!(metadata.uid(), owner.uid());
            assert_eq!(metadata.gid(), owner.gid());
        }
    }
}
