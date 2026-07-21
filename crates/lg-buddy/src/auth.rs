use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const BSCPYLGTV_OWNER_USER_ENV: &str = "LG_BUDDY_BSCPYLGTV_OWNER_USER";
pub const BSCPYLGTV_KEY_FILE_ENV: &str = "LG_BUDDY_BSCPYLGTV_KEY_FILE";
pub const DEFAULT_BSCPYLGTV_KEY_FILE_NAME: &str = ".aiopylgtv.sqlite";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUser {
    username: String,
    uid: u32,
    gid: u32,
    home: PathBuf,
}

impl SystemUser {
    pub fn new(username: impl Into<String>, uid: u32, gid: u32, home: impl Into<PathBuf>) -> Self {
        Self {
            username: username.into(),
            uid,
            gid,
            home: home.into(),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn home(&self) -> &Path {
        &self.home
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BscpylgtvAuthContext {
    owner: Option<SystemUser>,
    key_file_path: Option<PathBuf>,
}

impl BscpylgtvAuthContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_owner(mut self, owner: SystemUser) -> Self {
        self.owner = Some(owner);
        self
    }

    pub fn with_key_file_path(mut self, key_file_path: impl Into<PathBuf>) -> Self {
        self.key_file_path = Some(key_file_path.into());
        self
    }

    pub fn owner(&self) -> Option<&SystemUser> {
        self.owner.as_ref()
    }

    pub fn owner_user(&self) -> Option<&str> {
        self.owner().map(SystemUser::username)
    }

    pub fn key_file_path(&self) -> Option<&Path> {
        self.key_file_path.as_deref()
    }
}

#[derive(Debug)]
pub enum AuthContextError {
    ConfigPathHasNoParent { path: PathBuf },
    ConfigMetadata { path: PathBuf, source: io::Error },
    PasswdRead(io::Error),
    OwnerUserNotFound { user: String },
    ConfigOwnerNotFound { path: PathBuf, uid: u32 },
    UnsupportedPlatform(&'static str),
}

impl fmt::Display for AuthContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigPathHasNoParent { path } => {
                write!(
                    f,
                    "could not derive a TV auth path because config path `{}` has no parent directory",
                    path.display()
                )
            }
            Self::ConfigMetadata { path, source } => {
                write!(
                    f,
                    "could not read metadata for config path `{}`: {source}",
                    path.display()
                )
            }
            Self::PasswdRead(source) => write!(f, "could not read /etc/passwd: {source}"),
            Self::OwnerUserNotFound { user } => {
                write!(
                    f,
                    "could not resolve TV auth owner user `{user}` from /etc/passwd"
                )
            }
            Self::ConfigOwnerNotFound { path, uid } => write!(
                f,
                "could not resolve config owner uid `{uid}` for `{}` from /etc/passwd",
                path.display()
            ),
            Self::UnsupportedPlatform(reason) => write!(f, "{reason}"),
        }
    }
}

impl Error for AuthContextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ConfigMetadata { source, .. } | Self::PasswdRead(source) => Some(source),
            Self::ConfigPathHasNoParent { .. }
            | Self::OwnerUserNotFound { .. }
            | Self::ConfigOwnerNotFound { .. }
            | Self::UnsupportedPlatform(_) => None,
        }
    }
}

pub fn resolve_bscpylgtv_auth_context_from_env(
    config_path: &Path,
) -> Result<BscpylgtvAuthContext, AuthContextError> {
    let explicit_owner_user = env::var(BSCPYLGTV_OWNER_USER_ENV).ok();
    let explicit_key_file_path = env::var_os(BSCPYLGTV_KEY_FILE_ENV).map(PathBuf::from);
    let passwd = fs::read_to_string("/etc/passwd").map_err(AuthContextError::PasswdRead)?;

    let owner = match explicit_owner_user.as_deref() {
        Some(user) => parse_user_from_passwd_entries_by_name(&passwd, user).ok_or_else(|| {
            AuthContextError::OwnerUserNotFound {
                user: user.to_string(),
            }
        })?,
        None => resolve_config_owner_from_passwd(config_path, &passwd)?,
    };

    let key_file_path = match explicit_key_file_path {
        Some(path) => path,
        None => default_key_file_path(config_path)?,
    };

    Ok(BscpylgtvAuthContext::new()
        .with_owner(owner)
        .with_key_file_path(key_file_path))
}

pub fn resolve_config_owner(config_path: &Path) -> Result<SystemUser, AuthContextError> {
    let passwd = fs::read_to_string("/etc/passwd").map_err(AuthContextError::PasswdRead)?;
    resolve_config_owner_from_passwd(config_path, &passwd)
}

fn default_key_file_path(config_path: &Path) -> Result<PathBuf, AuthContextError> {
    let parent = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| AuthContextError::ConfigPathHasNoParent {
            path: config_path.to_path_buf(),
        })?;
    Ok(parent.join(DEFAULT_BSCPYLGTV_KEY_FILE_NAME))
}

fn resolve_config_owner_from_passwd(
    config_path: &Path,
    passwd_contents: &str,
) -> Result<SystemUser, AuthContextError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata =
            fs::metadata(config_path).map_err(|source| AuthContextError::ConfigMetadata {
                path: config_path.to_path_buf(),
                source,
            })?;
        let uid = metadata.uid();

        parse_user_from_passwd_entries_by_uid(passwd_contents, uid).ok_or_else(|| {
            AuthContextError::ConfigOwnerNotFound {
                path: config_path.to_path_buf(),
                uid,
            }
        })
    }

    #[cfg(not(unix))]
    {
        let _ = (config_path, passwd_contents);
        Err(AuthContextError::UnsupportedPlatform(
            "TV auth owner resolution is only supported on Unix platforms",
        ))
    }
}

fn parse_user_from_passwd_entries_by_name(contents: &str, user: &str) -> Option<SystemUser> {
    contents
        .lines()
        .filter_map(parse_passwd_entry)
        .find(|entry| entry.username == user)
        .map(PasswdEntry::into_system_user)
}

fn parse_user_from_passwd_entries_by_uid(contents: &str, uid: u32) -> Option<SystemUser> {
    contents
        .lines()
        .filter_map(parse_passwd_entry)
        .find(|entry| entry.uid == uid)
        .map(PasswdEntry::into_system_user)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasswdEntry {
    username: String,
    uid: u32,
    gid: u32,
    home: PathBuf,
}

impl PasswdEntry {
    fn into_system_user(self) -> SystemUser {
        SystemUser::new(self.username, self.uid, self.gid, self.home)
    }
}

fn parse_passwd_entry(line: &str) -> Option<PasswdEntry> {
    let mut fields = line.split(':');
    let username = fields.next()?;
    let _password = fields.next()?;
    let uid = fields.next()?.parse::<u32>().ok()?;
    let gid = fields.next()?.parse::<u32>().ok()?;
    let _gecos = fields.next()?;
    let home = fields.next()?;
    let _shell = fields.next()?;

    Some(PasswdEntry {
        username: username.to_string(),
        uid,
        gid,
        home: PathBuf::from(home),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        default_key_file_path, parse_user_from_passwd_entries_by_name,
        parse_user_from_passwd_entries_by_uid, AuthContextError, SystemUser,
        DEFAULT_BSCPYLGTV_KEY_FILE_NAME,
    };
    use std::path::Path;

    #[test]
    fn default_key_file_path_lives_next_to_config_env() {
        let path = default_key_file_path(Path::new("/home/vas/.config/lg-buddy/config.env"))
            .expect("derive default key file path");

        assert_eq!(
            path,
            Path::new("/home/vas/.config/lg-buddy").join(DEFAULT_BSCPYLGTV_KEY_FILE_NAME)
        );
    }

    #[test]
    fn default_key_file_path_requires_parent_directory() {
        let err = default_key_file_path(Path::new("config.env"))
            .expect_err("relative bare filename should not have a parent directory");

        assert!(matches!(
            err,
            AuthContextError::ConfigPathHasNoParent { .. }
        ));
    }

    #[test]
    fn passwd_lookup_by_name_returns_full_identity() {
        let passwd = "\
root:x:0:0:root:/root:/bin/bash\n\
vas:x:1000:1000:vas:/home/vas:/bin/bash\n";

        assert_eq!(
            parse_user_from_passwd_entries_by_name(passwd, "vas"),
            Some(SystemUser::new("vas", 1000, 1000, "/home/vas"))
        );
    }

    #[test]
    fn passwd_lookup_by_uid_returns_full_identity() {
        let passwd = "\
root:x:0:0:root:/root:/bin/bash\n\
vas:x:1000:1000:vas:/home/vas:/bin/bash\n";

        assert_eq!(
            parse_user_from_passwd_entries_by_uid(passwd, 0),
            Some(SystemUser::new("root", 0, 0, "/root"))
        );
    }
}
