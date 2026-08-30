use std::collections::BTreeSet;
use std::env;
use std::ffi::CString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const SYSTEM_FILES: &[(&str, bool)] = &[
    ("/usr/bin/lg-buddy", true),
    ("/etc/systemd/system/LG_Buddy.service", false),
    ("/etc/systemd/system/LG_Buddy.service.d/config.conf", false),
    ("/etc/systemd/system/LG_Buddy_lifecycle.service", false),
    (
        "/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf",
        false,
    ),
    ("/etc/tmpfiles.d/lg_buddy.conf", false),
    (
        "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle",
        true,
    ),
    ("/usr/share/applications/LG_Buddy_Brightness.desktop", false),
];

const SYSTEM_MUTABLE_DIRECTORIES: &[&str] = &[
    "/usr/bin",
    "/usr/bin/LG_Buddy_PIP",
    "/usr/lib/lg-buddy",
    "/etc/systemd/system",
    "/etc/systemd/system/LG_Buddy.service.d",
    "/etc/systemd/system/LG_Buddy_lifecycle.service.d",
    "/etc/tmpfiles.d",
    "/etc/NetworkManager/dispatcher.d/pre-down.d",
    "/usr/share/applications",
];

const LEGACY_SYSTEM_PATHS: &[&str] = &[
    "/usr/bin/LG_Buddy_Startup",
    "/usr/bin/LG_Buddy_Shutdown",
    "/usr/bin/LG_Buddy_Screen_On",
    "/usr/bin/LG_Buddy_Screen_Off",
    "/usr/bin/LG_Buddy_Screen_Monitor",
    "/usr/bin/LG_Buddy_sleep_pre",
    "/usr/bin/LG_Buddy_Brightness",
    "/usr/lib/lg-buddy/common.sh",
    "/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook",
    "/etc/systemd/system/LG_Buddy_wake.service",
    "/etc/systemd/system/LG_Buddy_wake.service.d",
    "/etc/systemd/system/LG_Buddy_sleep.service",
    "/etc/systemd/system/LG_Buddy_sleep.service.d",
    "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep",
];

const USER_SYSTEMD_FILES: &[(&str, bool)] = &[
    ("LG_Buddy_screen.service", false),
    ("LG_Buddy_screen.service.d/config.conf", false),
    ("LG_Buddy_update_check.service", false),
    ("LG_Buddy_update_check.service.d/config.conf", false),
    ("LG_Buddy_update_check.timer", false),
];

const USER_MUTABLE_DIRECTORIES: &[&str] = &[
    ".config/systemd/user",
    ".config/systemd/user/LG_Buddy_screen.service.d",
    ".config/systemd/user/LG_Buddy_update_check.service.d",
];

const CANDIDATE_FILES: &[(&str, bool)] = &[
    ("release-manifest.json", false),
    ("install.sh", true),
    ("lg-buddy", true),
    ("LG_Buddy_Brightness.desktop", false),
    ("systemd/LG_Buddy.service", false),
    ("systemd/LG_Buddy_lifecycle.service", false),
    ("systemd/LG_Buddy_screen.service", false),
    ("systemd/LG_Buddy_update_check.service", false),
    ("systemd/LG_Buddy_update_check.timer", false),
    ("systemd/lg_buddy.conf", false),
];

const MAX_CONFIG_TREE_ENTRIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFacts {
    pub kind: PathKind,
    pub owner_uid: u32,
    pub mode: u32,
    pub link_count: u64,
    pub read_only_filesystem: bool,
    pub mount_point: bool,
}

pub trait FilesystemFacts {
    fn path_facts(&self, path: &Path) -> io::Result<PathFacts>;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OsFilesystemFacts;

impl FilesystemFacts for OsFilesystemFacts {
    fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
        let metadata = fs::symlink_metadata(path)?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            PathKind::Symlink
        } else if file_type.is_file() {
            PathKind::File
        } else if file_type.is_dir() {
            PathKind::Directory
        } else {
            PathKind::Other
        };

        Ok(PathFacts {
            kind,
            owner_uid: metadata.uid(),
            mode: metadata.permissions().mode(),
            link_count: metadata.nlink(),
            read_only_filesystem: if matches!(kind, PathKind::File | PathKind::Directory) {
                filesystem_is_read_only(path)?
            } else {
                false
            },
            mount_point: if matches!(kind, PathKind::File | PathKind::Directory) {
                path_is_mount_point(path)?
            } else {
                false
            },
        })
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries: Vec<_> = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        entries.sort();
        Ok(entries)
    }
}

fn filesystem_is_read_only(path: &Path) -> io::Result<bool> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_flag & libc::ST_RDONLY as libc::c_ulong != 0)
}

fn path_is_mount_point(path: &Path) -> io::Result<bool> {
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    let path = path.as_os_str().as_bytes();
    Ok(mountinfo.split(|byte| *byte == b'\n').any(|line| {
        line.split(|byte| *byte == b' ')
            .nth(4)
            .is_some_and(|field| decode_mountinfo_field(field) == path)
    }))
}

fn decode_mountinfo_field(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && index + 3 < field.len()
            && field[index + 1..=index + 3]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = (field[index + 1] - b'0') * 64
                + (field[index + 2] - b'0') * 8
                + (field[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(field[index]);
            index += 1;
        }
    }
    decoded
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityFact {
    Available,
    Unavailable(String),
}

impl CapabilityFact {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable(reason.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerFacts {
    pub system: CapabilityFact,
    pub user: CapabilityFact,
}

impl ServiceManagerFacts {
    pub fn available() -> Self {
        Self {
            system: CapabilityFact::Available,
            user: CapabilityFact::Available,
        }
    }

    pub fn observe() -> Self {
        Self {
            system: observe_systemd(false),
            user: observe_systemd(true),
        }
    }
}

fn observe_systemd(user: bool) -> CapabilityFact {
    let mut command = Command::new("systemctl");
    if user {
        command.arg("--user");
    }
    let output = match command.arg("is-system-running").output() {
        Ok(output) => output,
        Err(err) => {
            return CapabilityFact::unavailable(format!("could not run systemctl: {err}"));
        }
    };
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if matches!(state.as_str(), "running" | "degraded") {
        CapabilityFact::Available
    } else if state.is_empty() {
        CapabilityFact::unavailable(format!(
            "systemctl did not report a usable manager state ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    } else {
        CapabilityFact::unavailable(format!("systemd manager state is {state}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLayout {
    pub system_root: PathBuf,
    pub user_home: PathBuf,
}

impl InstalledLayout {
    pub fn new(system_root: impl Into<PathBuf>, user_home: impl Into<PathBuf>) -> Self {
        Self {
            system_root: system_root.into(),
            user_home: user_home.into(),
        }
    }

    pub fn system_path(&self, path: &str) -> PathBuf {
        debug_assert!(path.starts_with('/'));
        if self.system_root == Path::new("/") {
            PathBuf::from(path)
        } else {
            self.system_root.join(path.trim_start_matches('/'))
        }
    }

    pub fn installed_executable(&self) -> PathBuf {
        self.system_path("/usr/bin/lg-buddy")
    }

    pub fn config_pointer(&self) -> PathBuf {
        self.system_path("/usr/lib/lg-buddy/config-path")
    }

    fn user_systemd_path(&self, path: &str) -> PathBuf {
        self.user_home.join(".config/systemd/user").join(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPreflightFacts {
    pub layout: InstalledLayout,
    pub running_executable: PathBuf,
    pub effective_uid: u32,
    pub system_owner_uid: u32,
    pub user_owner_uid: u32,
    pub service_managers: ServiceManagerFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityFailure {
    pub check: &'static str,
    pub path: Option<PathBuf>,
    pub detail: String,
    pub remedy: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatibilityReport {
    failures: Vec<CompatibilityFailure>,
}

impl CompatibilityReport {
    pub fn compatible(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn failures(&self) -> &[CompatibilityFailure] {
        &self.failures
    }

    pub fn render(&self) -> String {
        if self.compatible() {
            return "upgrade preflight: compatible\n".to_string();
        }

        let mut output = String::from("upgrade preflight: refused\n");
        for failure in &self.failures {
            output.push_str("- ");
            output.push_str(failure.check);
            if let Some(path) = &failure.path {
                output.push_str(" (");
                output.push_str(&path.display().to_string());
                output.push(')');
            }
            output.push_str(": ");
            output.push_str(&failure.detail);
            output.push_str(" Remedy: ");
            output.push_str(&failure.remedy);
            output.push('\n');
        }
        output
    }

    fn refuse(
        &mut self,
        check: &'static str,
        path: Option<PathBuf>,
        detail: impl Into<String>,
        remedy: impl Into<String>,
    ) {
        self.failures.push(CompatibilityFailure {
            check,
            path,
            detail: detail.into(),
            remedy: remedy.into(),
        });
    }

    fn extend(&mut self, other: Self) {
        self.failures.extend(other.failures);
    }
}

impl fmt::Display for CompatibilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

pub fn current_host_preflight() -> CompatibilityReport {
    let facts = match observe_current_process() {
        Ok(facts) => facts,
        Err(report) => return report,
    };
    evaluate_initial_preflight(&OsFilesystemFacts, &facts)
}

pub fn candidate_host_preflight(candidate_root: &Path) -> CompatibilityReport {
    let facts = match observe_current_process() {
        Ok(facts) => facts,
        Err(report) => return report,
    };
    evaluate_candidate_host_preflight(&OsFilesystemFacts, &facts, candidate_root)
}

fn observe_current_process() -> Result<HostPreflightFacts, CompatibilityReport> {
    let mut report = CompatibilityReport::default();
    let running_executable = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            report.refuse(
                "running-executable",
                None,
                format!("could not resolve the running executable: {err}"),
                "run the installed LG Buddy executable directly",
            );
            return Err(report);
        }
    };
    let user_home = match env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => {
            report.refuse(
                "user-home",
                None,
                "HOME is not available",
                "run the updater from the installed user's normal session",
            );
            return Err(report);
        }
    };
    let effective_uid = unsafe { libc::geteuid() };
    Ok(HostPreflightFacts {
        layout: InstalledLayout::new("/", user_home),
        running_executable,
        effective_uid,
        system_owner_uid: 0,
        user_owner_uid: effective_uid,
        service_managers: ServiceManagerFacts::observe(),
    })
}

pub fn evaluate_initial_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
) -> CompatibilityReport {
    evaluate_installed_state(
        filesystem,
        facts,
        &facts.layout.installed_executable(),
        "running-executable",
        "run the release-bundle installation at /usr/bin/lg-buddy, or use the host's native package manager",
    )
}

fn evaluate_installed_state(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    expected_running_executable: &Path,
    executable_check: &'static str,
    executable_remedy: &'static str,
) -> CompatibilityReport {
    let mut checker = Checker::new(filesystem);

    if facts.effective_uid == 0 {
        checker.report.refuse(
            "invoking-user",
            None,
            "the updater is running as root",
            "run LG Buddy as the installed user; it will request sudo only for the mutation step",
        );
    }
    check_normalized_absolute(
        &mut checker.report,
        "system-root",
        &facts.layout.system_root,
    );
    check_normalized_absolute(&mut checker.report, "user-home", &facts.layout.user_home);

    if facts.running_executable != expected_running_executable {
        checker.report.refuse(
            executable_check,
            Some(facts.running_executable.clone()),
            format!(
                "running executable is not the expected runtime at {}",
                expected_running_executable.display()
            ),
            executable_remedy,
        );
    }

    for (path, executable) in SYSTEM_FILES {
        checker.check_file(
            &facts.layout.system_path(path),
            facts.system_owner_uid,
            *executable,
            true,
            Some(&facts.layout.system_root),
            "installed-layout",
        );
    }
    for path in SYSTEM_MUTABLE_DIRECTORIES {
        checker.check_directory(
            &facts.layout.system_path(path),
            facts.system_owner_uid,
            true,
            Some(&facts.layout.system_root),
            "mutable-installation",
        );
    }
    for (path, executable) in USER_SYSTEMD_FILES {
        checker.check_file(
            &facts.layout.user_systemd_path(path),
            facts.user_owner_uid,
            *executable,
            true,
            None,
            "user-integration",
        );
    }
    for path in USER_MUTABLE_DIRECTORIES {
        checker.check_directory(
            &facts.layout.user_home.join(path),
            facts.user_owner_uid,
            true,
            None,
            "mutable-user-integration",
        );
    }

    for path in LEGACY_SYSTEM_PATHS {
        checker.check_absent(&facts.layout.system_path(path));
    }

    let config_path = checker.read_config_pointer(
        &facts.layout.config_pointer(),
        facts.system_owner_uid,
        &facts.layout.system_root,
    );
    if let Some(config_path) = config_path {
        let config_marker = systemd_config_override_line(&config_path);
        for path in [
            facts
                .layout
                .system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf"),
            facts
                .layout
                .system_path("/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"),
            facts
                .layout
                .user_systemd_path("LG_Buddy_screen.service.d/config.conf"),
            facts
                .layout
                .user_systemd_path("LG_Buddy_update_check.service.d/config.conf"),
        ] {
            checker.check_integration_override(&path, &config_marker);
        }
        checker.check_config_tree(&config_path, facts.user_owner_uid);
    }

    checker.check_capability(
        "system-service-manager",
        &facts.service_managers.system,
        "make the system systemd manager available before upgrading",
    );
    checker.check_capability(
        "user-service-manager",
        &facts.service_managers.user,
        "run the upgrade from a user session with a reachable systemd user manager",
    );

    checker.report
}

pub fn evaluate_candidate_preflight(
    filesystem: &impl FilesystemFacts,
    candidate_root: &Path,
    user_owner_uid: u32,
) -> CompatibilityReport {
    let mut checker = Checker::new(filesystem);
    if !check_normalized_absolute(&mut checker.report, "candidate-root", candidate_root) {
        return checker.report;
    }

    checker.check_directory(
        candidate_root,
        user_owner_uid,
        true,
        None,
        "candidate-layout",
    );
    checker.check_directory(
        &candidate_root.join("systemd"),
        user_owner_uid,
        false,
        None,
        "candidate-layout",
    );
    for (path, executable) in CANDIDATE_FILES {
        checker.check_file(
            &candidate_root.join(path),
            user_owner_uid,
            *executable,
            false,
            None,
            "candidate-layout",
        );
    }
    checker.check_owner_permissions(
        &candidate_root.join("lg-buddy"),
        0o100,
        "candidate runtime is not executable by its owner",
    );
    checker.check_owner_permissions(
        &candidate_root.join("install.sh"),
        0o500,
        "candidate installer is not readable and executable by its owner",
    );
    checker.report
}

pub fn evaluate_candidate_host_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    candidate_root: &Path,
) -> CompatibilityReport {
    let expected_candidate_executable = candidate_root.join("lg-buddy");
    let mut report = evaluate_installed_state(
        filesystem,
        facts,
        &expected_candidate_executable,
        "candidate-executable",
        "run the preflight with the verified candidate binary from this bundle",
    );
    report.extend(evaluate_candidate_preflight(
        filesystem,
        candidate_root,
        facts.user_owner_uid,
    ));
    report
}

fn check_normalized_absolute(
    report: &mut CompatibilityReport,
    check: &'static str,
    path: &Path,
) -> bool {
    let has_dot_component = path
        .as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|component| matches!(component, b"." | b".."));
    if !path.is_absolute() || has_dot_component {
        report.refuse(
            check,
            Some(path.to_path_buf()),
            "path is not normalized and absolute",
            "use an absolute path without '.' or '..' components",
        );
        false
    } else {
        true
    }
}

fn systemd_config_override_line(config_path: &Path) -> String {
    let escaped = config_path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("Environment=\"LG_BUDDY_CONFIG={escaped}\"")
}

struct Checker<'a, F> {
    filesystem: &'a F,
    report: CompatibilityReport,
    checked_ancestors: BTreeSet<(PathBuf, Option<u32>)>,
}

impl<'a, F: FilesystemFacts> Checker<'a, F> {
    fn new(filesystem: &'a F) -> Self {
        Self {
            filesystem,
            report: CompatibilityReport::default(),
            checked_ancestors: BTreeSet::new(),
        }
    }

    fn check_file(
        &mut self,
        path: &Path,
        owner_uid: u32,
        executable: bool,
        must_be_mutable: bool,
        trusted_system_root: Option<&Path>,
        check: &'static str,
    ) {
        self.check_ancestors(path, trusted_system_root, owner_uid);
        let facts = match self.path_facts(path, check) {
            Some(facts) => facts,
            None => return,
        };
        if facts.kind != PathKind::File {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a regular file, found {:?}", facts.kind),
                "restore this path from a current release-bundle installation",
            );
            return;
        }
        self.check_owner(path, &facts, owner_uid, check);
        if trusted_system_root.is_some() {
            self.check_not_writable_by_others(path, &facts);
        }
        if must_be_mutable && facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is on a read-only filesystem",
                "use the host's native package manager or make this installation file mutable",
            );
        }
        if must_be_mutable && facts.link_count != 1 {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!(
                    "file has {} hard links, expected exactly one",
                    facts.link_count
                ),
                "replace the path with an independent regular file before upgrading",
            );
        }
        if must_be_mutable && facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is a mount point",
                "replace the mounted path with an ordinary installation file before upgrading",
            );
        }
        if must_be_mutable && facts.mode & 0o200 == 0 {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is not writable by its owner",
                "restore owner-write permission before upgrading",
            );
        }
        if executable && facts.mode & 0o111 == 0 {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is not executable",
                "restore the executable mode from a current release bundle",
            );
        }
    }

    fn check_directory(
        &mut self,
        path: &Path,
        owner_uid: u32,
        must_be_mutable: bool,
        trusted_system_root: Option<&Path>,
        check: &'static str,
    ) {
        self.check_ancestors(path, trusted_system_root, owner_uid);
        let facts = match self.path_facts(path, check) {
            Some(facts) => facts,
            None => return,
        };
        if facts.kind != PathKind::Directory {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a directory, found {:?}", facts.kind),
                "restore this directory as part of a current release-bundle installation",
            );
            return;
        }
        self.check_owner(path, &facts, owner_uid, check);
        if trusted_system_root.is_some() {
            self.check_not_writable_by_others(path, &facts);
        }
        if must_be_mutable && facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is on a read-only filesystem",
                "use the host's native package manager or make the installed release-bundle paths mutable",
            );
        }
        if must_be_mutable && facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is a mount point",
                "replace the mounted path with an ordinary installation directory before upgrading",
            );
        }
        if must_be_mutable && facts.mode & 0o200 == 0 {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is not writable by its owner",
                "restore owner-write permission before upgrading",
            );
        }
    }

    fn check_integration_override(&mut self, path: &Path, expected: &str) {
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(path))
        {
            return;
        }
        match self.filesystem.read_to_string(path) {
            Ok(contents) => {
                let directives: Vec<_> = contents
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.starts_with('#') && !line.starts_with(';'))
                    .filter(|line| line.contains("LG_BUDDY_CONFIG"))
                    .collect();
                if directives.len() != 1 || directives[0] != expected {
                    self.report.refuse(
                        "integration-config",
                        Some(path.to_path_buf()),
                        format!("integration does not reference exactly {expected}"),
                        "restore the sole integration override for the discovered config path",
                    );
                }
            }
            Err(err) => self.report.refuse(
                "integration-config",
                Some(path.to_path_buf()),
                format!("could not read the integration override: {err}"),
                "restore a readable integration override",
            ),
        }

        let Some(directory) = path.parent() else {
            return;
        };
        match self.filesystem.read_directory(directory) {
            Ok(entries) => {
                for entry in entries {
                    if entry != path {
                        self.report.refuse(
                            "integration-config",
                            Some(entry),
                            "integration override directory contains an unexpected entry",
                            "remove unexpected drop-ins before upgrading",
                        );
                    }
                }
            }
            Err(err) => self.report.refuse(
                "integration-config",
                Some(directory.to_path_buf()),
                format!("could not inspect the integration override directory: {err}"),
                "make the integration override directory readable before upgrading",
            ),
        }
    }

    fn check_absent(&mut self, path: &Path) {
        match self.filesystem.path_facts(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => self.report.refuse(
                "legacy-layout",
                Some(path.to_path_buf()),
                format!("could not determine whether a legacy path exists: {err}"),
                "inspect and remove the legacy integration before upgrading",
            ),
            Ok(_) => self.report.refuse(
                "legacy-layout",
                Some(path.to_path_buf()),
                "legacy installation state is present",
                "reinstall the current release bundle cleanly; the updater does not migrate legacy layouts",
            ),
        }
    }

    fn read_config_pointer(
        &mut self,
        path: &Path,
        owner_uid: u32,
        system_root: &Path,
    ) -> Option<PathBuf> {
        self.check_file(
            path,
            owner_uid,
            false,
            true,
            Some(system_root),
            "config-discovery",
        );
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(path))
        {
            return None;
        }
        let contents = match self.filesystem.read_to_string(path) {
            Ok(contents) => contents,
            Err(err) => {
                self.report.refuse(
                    "config-discovery",
                    Some(path.to_path_buf()),
                    format!("could not read the installed config pointer: {err}"),
                    "restore the config pointer from a current release-bundle installation",
                );
                return None;
            }
        };
        let lines: Vec<_> = contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.len() != 1 {
            self.report.refuse(
                "config-discovery",
                Some(path.to_path_buf()),
                "config pointer must contain exactly one non-empty path",
                "rewrite the pointer with the absolute path to config.env",
            );
            return None;
        }
        let config_path = PathBuf::from(lines[0]);
        if !check_normalized_absolute(&mut self.report, "config-discovery", &config_path) {
            return None;
        }
        Some(config_path)
    }

    fn check_config_tree(&mut self, config_path: &Path, owner_uid: u32) {
        self.check_file(config_path, owner_uid, false, false, None, "config-state");
        let Some(config_directory) = config_path.parent() else {
            self.report.refuse(
                "config-state",
                Some(config_path.to_path_buf()),
                "config path has no parent directory",
                "place config.env in a user-owned configuration directory",
            );
            return;
        };
        self.check_directory(
            config_directory,
            owner_uid,
            true,
            None,
            "mutable-config-state",
        );
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(config_directory))
        {
            return;
        }

        let mut pending = vec![config_directory.to_path_buf()];
        let mut seen = 0;
        while let Some(directory) = pending.pop() {
            let entries = match self.filesystem.read_directory(&directory) {
                Ok(entries) => entries,
                Err(err) => {
                    self.report.refuse(
                        "config-state",
                        Some(directory),
                        format!("could not inspect the config directory: {err}"),
                        "make the LG Buddy config directory readable by the installed user",
                    );
                    return;
                }
            };
            for entry in entries {
                seen += 1;
                if seen > MAX_CONFIG_TREE_ENTRIES {
                    self.report.refuse(
                        "config-state",
                        Some(config_directory.to_path_buf()),
                        format!("config tree exceeds {MAX_CONFIG_TREE_ENTRIES} entries"),
                        "remove unrelated files from the LG Buddy config directory",
                    );
                    return;
                }
                let facts = match self.path_facts(&entry, "config-state") {
                    Some(facts) => facts,
                    None => continue,
                };
                self.check_owner(&entry, &facts, owner_uid, "config-state");
                match facts.kind {
                    PathKind::Directory => pending.push(entry),
                    PathKind::File => {}
                    PathKind::Symlink | PathKind::Other => self.report.refuse(
                        "config-state",
                        Some(entry),
                        format!("config tree contains an unsafe {:?} entry", facts.kind),
                        "replace the entry with a user-owned regular file or directory",
                    ),
                }
            }
        }
    }

    fn check_capability(
        &mut self,
        check: &'static str,
        capability: &CapabilityFact,
        remedy: &'static str,
    ) {
        if let CapabilityFact::Unavailable(reason) = capability {
            self.report
                .refuse(check, None, reason.clone(), remedy.to_string());
        }
    }

    fn check_owner_permissions(&mut self, path: &Path, required: u32, detail: &'static str) {
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(path))
        {
            return;
        }
        let Some(facts) = self.path_facts(path, "candidate-layout") else {
            return;
        };
        if facts.mode & required != required {
            self.report.refuse(
                "candidate-layout",
                Some(path.to_path_buf()),
                detail,
                "restore the candidate file mode from a verified release bundle",
            );
        }
    }

    fn check_not_writable_by_others(&mut self, path: &Path, facts: &PathFacts) {
        if facts.mode & 0o022 != 0 {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                "system path is writable by its group or by other users",
                "remove group and other write permission from the system installation path",
            );
        }
    }

    fn check_ancestors(&mut self, path: &Path, trusted_system_root: Option<&Path>, owner_uid: u32) {
        for ancestor in path.ancestors().skip(1) {
            let trusted_owner = match trusted_system_root {
                Some(root) if ancestor.starts_with(root) => Some(owner_uid),
                _ => None,
            };
            if !self
                .checked_ancestors
                .insert((ancestor.to_path_buf(), trusted_owner))
            {
                continue;
            }
            match self.filesystem.path_facts(ancestor) {
                Ok(facts) if facts.kind == PathKind::Directory => {
                    if let Some(expected_uid) = trusted_owner {
                        self.check_owner(ancestor, &facts, expected_uid, "path-containment");
                        self.check_not_writable_by_others(ancestor, &facts);
                    }
                }
                Ok(facts) => self.report.refuse(
                    "path-containment",
                    Some(ancestor.to_path_buf()),
                    format!("path ancestor is {:?}, not a real directory", facts.kind),
                    "replace symlinked or special ancestors with ordinary directories",
                ),
                Err(err) => self.report.refuse(
                    "path-containment",
                    Some(ancestor.to_path_buf()),
                    format!("could not inspect path ancestor: {err}"),
                    "restore the complete installation path",
                ),
            }
        }
    }

    fn path_facts(&mut self, path: &Path, check: &'static str) -> Option<PathFacts> {
        match self.filesystem.path_facts(path) {
            Ok(facts) => Some(facts),
            Err(err) => {
                self.report.refuse(
                    check,
                    Some(path.to_path_buf()),
                    if err.kind() == io::ErrorKind::NotFound {
                        "required path is missing".to_string()
                    } else {
                        format!("could not inspect required path: {err}")
                    },
                    "restore this path from a current release-bundle installation",
                );
                None
            }
        }
    }

    fn check_owner(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        expected_uid: u32,
        check: &'static str,
    ) {
        if facts.owner_uid != expected_uid {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!(
                    "path is owned by uid {}, expected uid {expected_uid}",
                    facts.owner_uid
                ),
                "restore the expected ownership before upgrading",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn supported_release_bundle_layout_passes_initial_and_candidate_preflights() {
        let fixture = InstalledFixture::new("supported");

        let initial = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");
        let candidate = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
        );

        assert!(initial.compatible(), "{}", initial.render());
        assert!(candidate.compatible(), "{}", candidate.render());
    }

    #[test]
    fn os_filesystem_identifies_mount_points() {
        let root = OsFilesystemFacts.path_facts(Path::new("/")).unwrap();

        assert!(root.mount_point);
        assert_eq!(
            decode_mountinfo_field(br"/tmp/lg\040buddy\134config"),
            br"/tmp/lg buddy\config"
        );
    }

    #[test]
    fn initial_preflight_refuses_a_symlinked_installed_runtime() {
        let fixture = InstalledFixture::new("symlink-runtime");
        let runtime = fixture.facts.layout.installed_executable();
        fs::remove_file(&runtime).unwrap();
        symlink(fixture.candidate_root.join("lg-buddy"), &runtime).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &runtime, "Symlink");
    }

    #[test]
    fn initial_preflight_refuses_a_symlinked_installed_virtualenv() {
        let fixture = InstalledFixture::new("symlink-virtualenv");
        let virtualenv = fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP");
        let target = fixture.root.join("external-virtualenv");
        fs::remove_dir(&virtualenv).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&target, &virtualenv).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "mutable-installation", &virtualenv, "Symlink");
    }

    #[test]
    fn initial_preflight_refuses_an_incomplete_integration() {
        let fixture = InstalledFixture::new("missing-integration");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy_lifecycle.service");
        fs::remove_file(&service).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "missing");
    }

    #[test]
    fn initial_preflight_refuses_conflicting_ownership() {
        let fixture = InstalledFixture::new("wrong-owner");
        let runtime = fixture.facts.layout.installed_executable();
        let filesystem = OverriddenFilesystem {
            path: runtime.clone(),
            owner_uid: Some(fixture.facts.system_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "installed-layout", &runtime, "owned by uid");
    }

    #[test]
    fn initial_preflight_refuses_an_untrusted_system_ancestor() {
        let fixture = InstalledFixture::new("wrong-ancestor-owner");
        let ancestor = fixture.facts.layout.system_path("/etc/systemd");
        let filesystem = OverriddenFilesystem {
            path: ancestor.clone(),
            owner_uid: Some(fixture.facts.system_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "path-containment", &ancestor, "owned by uid");
    }

    #[test]
    fn initial_preflight_refuses_a_system_path_writable_by_other_users() {
        let fixture = InstalledFixture::new("world-writable-system-path");
        let directory = fixture.facts.layout.system_path("/etc/systemd/system");
        let filesystem = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: None,
            mode: Some(0o777),
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "path-containment",
            &directory,
            "writable by its group or by other users",
        );
    }

    #[test]
    fn initial_preflight_refuses_read_only_installation_paths() {
        let fixture = InstalledFixture::new("read-only");
        let directory = fixture.facts.layout.system_path("/usr/bin");
        let filesystem = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: None,
            mode: None,
            read_only: Some(true),
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "mutable-installation",
            &directory,
            "read-only filesystem",
        );
    }

    #[test]
    fn initial_preflight_refuses_a_read_only_installed_file() {
        let fixture = InstalledFixture::new("read-only-file");
        let runtime = fixture.facts.layout.installed_executable();
        let filesystem = OverriddenFilesystem {
            path: runtime.clone(),
            owner_uid: None,
            mode: None,
            read_only: Some(true),
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "installed-layout",
            &runtime,
            "read-only filesystem",
        );
    }

    #[test]
    fn initial_preflight_refuses_a_mounted_mutation_target() {
        let fixture = InstalledFixture::new("mounted-service");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        let filesystem = OverriddenFilesystem {
            path: service.clone(),
            owner_uid: None,
            mode: None,
            read_only: None,
            mount_point: Some(true),
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "mount point");
    }

    #[test]
    fn initial_preflight_refuses_a_hard_linked_mutation_target() {
        let fixture = InstalledFixture::new("hard-linked-service");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        fs::hard_link(&service, fixture.root.join("shared-service-file")).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "2 hard links");
    }

    #[test]
    fn initial_preflight_refuses_a_non_writable_mutation_target() {
        let fixture = InstalledFixture::new("non-writable-user-service");
        let service = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service");
        let mut permissions = fs::metadata(&service).unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&service, permissions).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "user-integration", &service, "not writable");
    }

    #[test]
    fn initial_preflight_refuses_unavailable_service_manager() {
        let mut fixture = InstalledFixture::new("no-user-manager");
        fixture.facts.service_managers.user =
            CapabilityFact::unavailable("user systemd manager is offline");

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        let failure = report
            .failures()
            .iter()
            .find(|failure| failure.check == "user-service-manager")
            .expect("user manager refusal");
        assert!(failure.detail.contains("offline"));
        assert!(failure.remedy.contains("user session"));
    }

    #[test]
    fn initial_preflight_refuses_integration_pointing_at_another_config() {
        let fixture = InstalledFixture::new("stale-config-override");
        let override_path = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service.d/config.conf");
        fs::write(
            &override_path,
            "[Service]\nEnvironment=\"LG_BUDDY_CONFIG=/tmp/other/config.env\"\n",
        )
        .unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &override_path,
            "does not reference",
        );
    }

    #[test]
    fn initial_preflight_refuses_conflicting_config_assignments() {
        let fixture = InstalledFixture::new("duplicate-config-override");
        let override_path = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service.d/config.conf");
        let mut contents = fs::read_to_string(&override_path).unwrap();
        contents.push_str("Environment=\"LG_BUDDY_CONFIG=/tmp/other/config.env\"\n");
        fs::write(&override_path, contents).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &override_path,
            "does not reference exactly",
        );
    }

    #[test]
    fn initial_preflight_refuses_an_unexpected_systemd_drop_in() {
        let fixture = InstalledFixture::new("unexpected-systemd-drop-in");
        let drop_in = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service.d/99-local.conf");
        write_file(&drop_in, false);

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "integration-config", &drop_in, "unexpected entry");
    }

    #[test]
    fn initial_preflight_refuses_legacy_state_instead_of_migrating_it() {
        let fixture = InstalledFixture::new("legacy-state");
        let legacy = fixture
            .facts
            .layout
            .system_path("/usr/bin/LG_Buddy_Startup");
        write_file(&legacy, false);

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "legacy-layout", &legacy, "legacy");
    }

    #[test]
    fn initial_preflight_refuses_a_legacy_override_directory() {
        let fixture = InstalledFixture::new("legacy-override-directory");
        let legacy = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy_wake.service.d");
        let external = fixture.root.join("external-legacy-override");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("config.conf"), "external\n").unwrap();
        symlink(&external, &legacy).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "legacy-layout", &legacy, "legacy");
    }

    #[test]
    fn initial_preflight_refuses_symlinks_in_config_state() {
        let fixture = InstalledFixture::new("config-symlink");
        let link = fixture.config_directory.join("linked-token.json");
        symlink(fixture.config_directory.join("config.env"), &link).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "config-state", &link, "unsafe Symlink");
    }

    #[test]
    fn candidate_preflight_refuses_missing_or_non_executable_inputs() {
        let fixture = InstalledFixture::new("bad-candidate");
        let manifest = fixture.candidate_root.join("release-manifest.json");
        let installer = fixture.candidate_root.join("install.sh");
        fs::remove_file(&manifest).unwrap();
        set_executable(&installer, false);

        let report = evaluate_candidate_preflight(
            &OsFilesystemFacts,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(&report, "candidate-layout", &manifest, "missing");
        assert_failure(&report, "candidate-layout", &installer, "not executable");
    }

    #[test]
    fn candidate_preflight_requires_owner_usable_executable_modes() {
        for (label, mode) in [("unreadable-installer", 0o100), ("other-executable", 0o401)] {
            let fixture = InstalledFixture::new(label);
            let installer = fixture.candidate_root.join("install.sh");
            set_mode(&installer, mode);

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(
                &report,
                "candidate-layout",
                &installer,
                "not readable and executable by its owner",
            );
        }
    }

    #[test]
    fn candidate_preflight_refuses_a_missing_upgrade_asset() {
        let fixture = InstalledFixture::new("missing-candidate-service");
        let service = fixture
            .candidate_root
            .join("systemd/LG_Buddy_screen.service");
        fs::remove_file(&service).unwrap();

        let report = evaluate_candidate_preflight(
            &OsFilesystemFacts,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(&report, "candidate-layout", &service, "missing");
    }

    #[test]
    fn candidate_host_preflight_must_run_from_the_verified_bundle() {
        let fixture = InstalledFixture::new("wrong-candidate-runtime");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &fixture.facts,
            &fixture.candidate_root,
        );

        let failure = report
            .failures()
            .iter()
            .find(|failure| failure.check == "candidate-executable")
            .expect("candidate executable refusal");
        assert!(failure.detail.contains("candidate/lg-buddy"));
    }

    #[test]
    fn candidate_host_preflight_rechecks_installed_state() {
        let fixture = InstalledFixture::new("candidate-rechecks-installed-state");
        let installed_service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        fs::remove_file(&installed_service).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
        );

        assert_failure(&report, "installed-layout", &installed_service, "missing");
    }

    #[test]
    fn candidate_preflight_refuses_unnormalized_relative_root() {
        let report =
            evaluate_candidate_preflight(&OsFilesystemFacts, Path::new("bundle/../next"), 1);

        let failure = report.failures().first().expect("candidate root refusal");
        assert_eq!(failure.check, "candidate-root");
        assert!(failure.detail.contains("normalized and absolute"));
    }

    #[test]
    fn initial_preflight_refuses_root_invocation() {
        let mut fixture = InstalledFixture::new("root-invocation");
        fixture.facts.effective_uid = 0;

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert!(report
            .failures()
            .iter()
            .any(|failure| failure.check == "invoking-user"));
    }

    fn assert_failure(report: &CompatibilityReport, check: &str, path: &Path, detail: &str) {
        let failure = report
            .failures()
            .iter()
            .find(|failure| failure.check == check && failure.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("missing {check} failure for {}:\n{report}", path.display()));
        assert!(
            failure.detail.contains(detail),
            "expected detail {detail:?}, got {:?}",
            failure.detail
        );
        assert!(!failure.remedy.is_empty());
    }

    struct OverriddenFilesystem {
        path: PathBuf,
        owner_uid: Option<u32>,
        mode: Option<u32>,
        read_only: Option<bool>,
        mount_point: Option<bool>,
    }

    impl FilesystemFacts for OverriddenFilesystem {
        fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
            let mut facts = OsFilesystemFacts.path_facts(path)?;
            if path == self.path {
                if let Some(owner_uid) = self.owner_uid {
                    facts.owner_uid = owner_uid;
                }
                if let Some(mode) = self.mode {
                    facts.mode = mode;
                }
                if let Some(read_only) = self.read_only {
                    facts.read_only_filesystem = read_only;
                }
                if let Some(mount_point) = self.mount_point {
                    facts.mount_point = mount_point;
                }
            }
            Ok(facts)
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            OsFilesystemFacts.read_to_string(path)
        }

        fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            OsFilesystemFacts.read_directory(path)
        }
    }

    struct InstalledFixture {
        root: PathBuf,
        facts: HostPreflightFacts,
        candidate_root: PathBuf,
        config_directory: PathBuf,
    }

    impl InstalledFixture {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let root = env::temp_dir().join(format!(
                "lg-buddy-upgrade-preflight-{label}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let system_root = root.join("root");
            let user_home = root.join("home/user");
            let layout = InstalledLayout::new(&system_root, &user_home);
            let config_directory = user_home.join(".config/lg-buddy");
            let config_path = config_directory.join("config.env");

            for path in SYSTEM_MUTABLE_DIRECTORIES {
                fs::create_dir_all(layout.system_path(path)).unwrap();
            }
            for (path, executable) in SYSTEM_FILES {
                write_file(&layout.system_path(path), *executable);
            }
            set_directory_tree_mode(&system_root, 0o755);
            for path in USER_MUTABLE_DIRECTORIES {
                fs::create_dir_all(user_home.join(path)).unwrap();
            }
            for (path, executable) in USER_SYSTEMD_FILES {
                write_file(&layout.user_systemd_path(path), *executable);
            }
            fs::create_dir_all(config_directory.join("tvs/primary")).unwrap();
            fs::write(&config_path, "updates_channel=stable\n").unwrap();
            fs::write(
                config_directory.join("tvs/primary/access-token.json"),
                "{}\n",
            )
            .unwrap();
            write_file(&layout.config_pointer(), false);
            fs::write(
                layout.config_pointer(),
                format!("{}\n", config_path.display()),
            )
            .unwrap();
            let config_override = format!(
                "[Service]\nEnvironment=\"LG_BUDDY_CONFIG={}\"\n",
                config_path.display()
            );
            for path in [
                layout.system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf"),
                layout.system_path("/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"),
                layout.user_systemd_path("LG_Buddy_screen.service.d/config.conf"),
                layout.user_systemd_path("LG_Buddy_update_check.service.d/config.conf"),
            ] {
                fs::write(path, &config_override).unwrap();
            }

            let candidate_root = root.join("candidate");
            fs::create_dir_all(&candidate_root).unwrap();
            for (path, executable) in CANDIDATE_FILES {
                write_file(&candidate_root.join(path), *executable);
            }

            let owner_uid = unsafe { libc::geteuid() };
            let facts = HostPreflightFacts {
                running_executable: layout.installed_executable(),
                layout,
                effective_uid: if owner_uid == 0 { 1000 } else { owner_uid },
                system_owner_uid: owner_uid,
                user_owner_uid: owner_uid,
                service_managers: ServiceManagerFacts::available(),
            };

            Self {
                root,
                facts,
                candidate_root,
                config_directory,
            }
        }
    }

    impl Drop for InstalledFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_file(path: &Path, executable: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture\n").unwrap();
        set_executable(path, executable);
    }

    fn set_directory_tree_mode(path: &Path, mode: u32) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                set_directory_tree_mode(&entry.path(), mode);
            }
        }
        set_mode(path, mode);
    }

    fn set_executable(path: &Path, executable: bool) {
        let mode = if executable { 0o755 } else { 0o644 };
        set_mode(path, mode);
    }

    fn set_mode(path: &Path, mode: u32) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).unwrap();
    }
}
