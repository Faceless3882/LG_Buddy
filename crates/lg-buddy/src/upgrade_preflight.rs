use std::collections::BTreeSet;
use std::env;
use std::ffi::{CString, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerPathPolicy {
    ReplaceFile,
    ReplaceExecutable,
    MutateDirectory,
    RecursiveClear,
    ExactDropInDirectory { expected_entry: &'static str },
    ReadableInput,
    SystemReadableInput,
    ExecutableInput,
    InputDirectory,
}

impl InstallerPathPolicy {
    fn expects_file(self) -> bool {
        matches!(
            self,
            Self::ReplaceFile
                | Self::ReplaceExecutable
                | Self::ReadableInput
                | Self::SystemReadableInput
                | Self::ExecutableInput
        )
    }

    fn expects_directory(self) -> bool {
        matches!(
            self,
            Self::MutateDirectory
                | Self::RecursiveClear
                | Self::ExactDropInDirectory { .. }
                | Self::InputDirectory
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstallerPathRequirement {
    path: &'static str,
    policy: InstallerPathPolicy,
}

const fn requirement(path: &'static str, policy: InstallerPathPolicy) -> InstallerPathRequirement {
    InstallerPathRequirement { path, policy }
}

const SYSTEM_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("/usr/bin/lg-buddy", InstallerPathPolicy::ReplaceExecutable),
    requirement(
        "/etc/systemd/system/LG_Buddy.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/tmpfiles.d/lg_buddy.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle",
        InstallerPathPolicy::ReplaceExecutable,
    ),
    requirement(
        "/usr/share/applications/LG_Buddy_Brightness.desktop",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement("/usr/bin", InstallerPathPolicy::MutateDirectory),
    requirement("/etc/systemd/system", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/etc/systemd/system/LG_Buddy.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement("/etc/tmpfiles.d", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/etc/NetworkManager/dispatcher.d/pre-down.d",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/applications",
        InstallerPathPolicy::MutateDirectory,
    ),
];

const PYTHON_REPAIR_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[requirement(
    "/usr/bin/LG_Buddy_PIP",
    InstallerPathPolicy::RecursiveClear,
)];

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

const USER_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("LG_Buddy_screen.service", InstallerPathPolicy::ReplaceFile),
    requirement(
        "LG_Buddy_screen.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.timer",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement("", InstallerPathPolicy::MutateDirectory),
    requirement(
        "LG_Buddy_screen.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement(
        "LG_Buddy_update_check.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
];

// These are the inputs consumed by the non-interactive `install.sh --upgrade`
// contract. Configuration and pairing scripts are deliberately not upgrade inputs.
const CANDIDATE_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("", InstallerPathPolicy::InputDirectory),
    requirement("systemd", InstallerPathPolicy::InputDirectory),
    requirement("release-manifest.json", InstallerPathPolicy::ReadableInput),
    requirement("install.sh", InstallerPathPolicy::ExecutableInput),
    requirement("lg-buddy", InstallerPathPolicy::ExecutableInput),
    requirement(
        "LG_Buddy_Brightness.desktop",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_lifecycle.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_screen.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_update_check.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_update_check.timer",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement("systemd/lg_buddy.conf", InstallerPathPolicy::ReadableInput),
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
    fn mount_points(&self) -> io::Result<Vec<PathBuf>>;
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
                mounted_paths()?.iter().any(|mounted| mounted == path)
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

    fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
        mounted_paths()
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

fn mounted_paths() -> io::Result<Vec<PathBuf>> {
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    Ok(mountinfo
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.split(|byte| *byte == b' ').nth(4))
        .map(|field| PathBuf::from(OsString::from_vec(decode_mountinfo_field(field))))
        .collect())
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

    fn user_desktop_entry(&self) -> PathBuf {
        self.user_home.join("Desktop/LG_Buddy_Brightness.desktop")
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

pub fn candidate_host_preflight(candidate_root: &Path, repair_python: bool) -> CompatibilityReport {
    let facts = match observe_current_process() {
        Ok(facts) => facts,
        Err(report) => return report,
    };
    evaluate_candidate_host_preflight(&OsFilesystemFacts, &facts, candidate_root, repair_python)
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
    let install_root = env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    let sandboxed_install = install_root.is_some();
    let system_root = install_root.unwrap_or_else(|| PathBuf::from("/"));
    let service_managers =
        if sandboxed_install && env::var("LG_BUDDY_SKIP_SYSTEMD_ACTIONS").as_deref() == Ok("1") {
            ServiceManagerFacts::available()
        } else {
            ServiceManagerFacts::observe()
        };
    Ok(HostPreflightFacts {
        layout: InstalledLayout::new(system_root, user_home),
        running_executable,
        effective_uid,
        system_owner_uid: if sandboxed_install { effective_uid } else { 0 },
        user_owner_uid: effective_uid,
        service_managers,
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
        false,
    )
}

fn evaluate_installed_state(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    expected_running_executable: &Path,
    executable_check: &'static str,
    executable_remedy: &'static str,
    repair_python: bool,
) -> CompatibilityReport {
    let mut checker = Checker::new(filesystem);
    let system_trust = TrustedRoot::strict(&facts.layout.system_root, facts.system_owner_uid);
    let user_trust = TrustedRoot::owned(&facts.layout.user_home, facts.user_owner_uid);

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

    for requirement in SYSTEM_PATH_REQUIREMENTS {
        let check = match requirement.policy {
            InstallerPathPolicy::MutateDirectory | InstallerPathPolicy::RecursiveClear => {
                "mutable-installation"
            }
            InstallerPathPolicy::ExactDropInDirectory { .. } => "integration-config",
            _ => "installed-layout",
        };
        checker.check_requirement(
            &facts.layout.system_path(requirement.path),
            facts.system_owner_uid,
            Some(system_trust),
            requirement.policy,
            check,
        );
    }
    if repair_python {
        for requirement in PYTHON_REPAIR_PATH_REQUIREMENTS {
            checker.check_requirement(
                &facts.layout.system_path(requirement.path),
                facts.system_owner_uid,
                Some(system_trust),
                requirement.policy,
                "python-environment-repair",
            );
        }
    }
    for requirement in USER_PATH_REQUIREMENTS {
        let check = match requirement.policy {
            InstallerPathPolicy::MutateDirectory => "mutable-user-integration",
            InstallerPathPolicy::ExactDropInDirectory { .. } => "integration-config",
            _ => "user-integration",
        };
        checker.check_requirement(
            &facts.layout.user_systemd_path(requirement.path),
            facts.user_owner_uid,
            Some(user_trust),
            requirement.policy,
            check,
        );
    }
    checker.check_optional_requirement(
        &facts.layout.user_desktop_entry(),
        facts.user_owner_uid,
        Some(user_trust),
        InstallerPathPolicy::ReplaceFile,
        "user-desktop",
    );

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

    let candidate_trust = TrustedRoot::candidate(candidate_root, user_owner_uid);
    for requirement in CANDIDATE_PATH_REQUIREMENTS {
        checker.check_requirement(
            &candidate_root.join(requirement.path),
            user_owner_uid,
            Some(candidate_trust),
            requirement.policy,
            "candidate-layout",
        );
    }
    checker.report
}

pub fn evaluate_candidate_host_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    candidate_root: &Path,
    repair_python: bool,
) -> CompatibilityReport {
    let expected_candidate_executable = candidate_root.join("lg-buddy");
    let mut report = evaluate_installed_state(
        filesystem,
        facts,
        &expected_candidate_executable,
        "candidate-executable",
        "run the preflight with the verified candidate binary from this bundle",
        repair_python,
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

#[derive(Debug, Clone, Copy)]
struct TrustedRoot<'a> {
    path: &'a Path,
    owner_uid: u32,
    reject_other_writes: bool,
    protect_external_ancestors: bool,
}

impl<'a> TrustedRoot<'a> {
    fn strict(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: true,
            protect_external_ancestors: false,
        }
    }

    fn candidate(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: true,
            protect_external_ancestors: true,
        }
    }

    fn owned(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: false,
            protect_external_ancestors: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AncestorPolicy {
    Trusted {
        owner_uid: u32,
        reject_other_writes: bool,
    },
    CandidateExternal {
        user_owner_uid: u32,
    },
}

struct Checker<'a, F> {
    filesystem: &'a F,
    report: CompatibilityReport,
    checked_ancestors: BTreeSet<(PathBuf, Option<AncestorPolicy>)>,
}

impl<'a, F: FilesystemFacts> Checker<'a, F> {
    fn new(filesystem: &'a F) -> Self {
        Self {
            filesystem,
            report: CompatibilityReport::default(),
            checked_ancestors: BTreeSet::new(),
        }
    }

    fn check_requirement(
        &mut self,
        path: &Path,
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        policy: InstallerPathPolicy,
        check: &'static str,
    ) {
        self.check_ancestors(path, trusted_root);
        let facts = match self.filesystem.path_facts(path) {
            Ok(facts) => facts,
            Err(err)
                if policy == InstallerPathPolicy::RecursiveClear
                    && err.kind() == io::ErrorKind::NotFound =>
            {
                return;
            }
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
                return;
            }
        };
        if policy.expects_file() && facts.kind != PathKind::File {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a regular file, found {:?}", facts.kind),
                "restore this path from a current release-bundle installation",
            );
            return;
        }
        if policy.expects_directory() && facts.kind != PathKind::Directory {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a directory, found {:?}", facts.kind),
                "restore this directory as part of a current release-bundle installation",
            );
            return;
        }
        self.check_owner(path, &facts, owner_uid, check);
        if trusted_root.is_some_and(|root| root.reject_other_writes) {
            self.check_not_writable_by_others(path, &facts);
        }

        match policy {
            InstallerPathPolicy::ReplaceFile => self.check_replace_file(path, &facts, check),
            InstallerPathPolicy::ReplaceExecutable => {
                self.check_replace_file(path, &facts, check);
                self.check_permissions(
                    path,
                    &facts,
                    0o111,
                    check,
                    "installed executable has no execute permission",
                );
            }
            InstallerPathPolicy::MutateDirectory => {
                self.check_mutable_directory(path, &facts, check, 0o300);
            }
            InstallerPathPolicy::RecursiveClear => {
                self.check_mutable_directory(path, &facts, check, 0o300);
                self.check_recursive_clear_mounts(path, check);
            }
            InstallerPathPolicy::ExactDropInDirectory { expected_entry } => {
                self.check_mutable_directory(path, &facts, check, 0o700);
                self.check_exact_directory(path, expected_entry, check);
            }
            InstallerPathPolicy::ReadableInput => self.check_permissions(
                path,
                &facts,
                0o400,
                check,
                "input is not readable by its owner",
            ),
            InstallerPathPolicy::SystemReadableInput => self.check_permissions(
                path,
                &facts,
                0o404,
                check,
                "system input is not readable by its owner and the invoking user",
            ),
            InstallerPathPolicy::ExecutableInput => self.check_permissions(
                path,
                &facts,
                0o500,
                check,
                "input is not readable and executable by its owner",
            ),
            InstallerPathPolicy::InputDirectory => self.check_permissions(
                path,
                &facts,
                0o500,
                check,
                "input directory is not readable and searchable by its owner",
            ),
        }
    }

    fn check_optional_requirement(
        &mut self,
        path: &Path,
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        policy: InstallerPathPolicy,
        check: &'static str,
    ) {
        match self.filesystem.path_facts(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            _ => self.check_requirement(path, owner_uid, trusted_root, policy, check),
        }
    }

    fn check_replace_file(&mut self, path: &Path, facts: &PathFacts, check: &'static str) {
        if facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is on a read-only filesystem",
                "use the host's native package manager or make this installation file mutable",
            );
        }
        if facts.link_count != 1 {
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
        if facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is a mount point",
                "replace the mounted path with an ordinary installation file before upgrading",
            );
        }
        self.check_permissions(
            path,
            facts,
            0o200,
            check,
            "file is not writable by its owner",
        );
    }

    fn check_mutable_directory(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        check: &'static str,
        required_permissions: u32,
    ) {
        if facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is on a read-only filesystem",
                "use the host's native package manager or make the installed release-bundle paths mutable",
            );
        }
        if facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is a mount point",
                "replace the mounted path with an ordinary installation directory before upgrading",
            );
        }
        self.check_permissions(
            path,
            facts,
            required_permissions,
            check,
            if required_permissions & 0o400 != 0 {
                "directory is not readable, writable, and searchable by its owner"
            } else {
                "directory is not writable and searchable by its owner"
            },
        );
    }

    fn check_recursive_clear_mounts(&mut self, path: &Path, check: &'static str) {
        match self.filesystem.mount_points() {
            Ok(mount_points) => {
                for mount_point in mount_points {
                    if mount_point != path && mount_point.starts_with(path) {
                        self.report.refuse(
                            check,
                            Some(mount_point),
                            "recursively cleared directory contains a nested mount point",
                            "unmount nested filesystems from the managed virtualenv before upgrading",
                        );
                    }
                }
            }
            Err(err) => self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("could not inspect nested mount points: {err}"),
                "make mount information available before upgrading",
            ),
        }
    }

    fn check_exact_directory(&mut self, path: &Path, expected_entry: &str, check: &'static str) {
        let expected_path = path.join(expected_entry);
        match self.filesystem.read_directory(path) {
            Ok(entries) => {
                if !entries.iter().any(|entry| entry == &expected_path) {
                    self.report.refuse(
                        check,
                        Some(expected_path.clone()),
                        "required drop-in entry is missing",
                        "restore the exact drop-in directory from a current release bundle",
                    );
                }
                for entry in entries {
                    if entry != expected_path {
                        self.report.refuse(
                            check,
                            Some(entry),
                            "drop-in directory contains an unexpected entry",
                            "remove unexpected drop-ins before upgrading",
                        );
                    }
                }
            }
            Err(err) => self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("could not inspect drop-in directory: {err}"),
                "make the drop-in directory readable before upgrading",
            ),
        }
    }

    fn check_permissions(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        required: u32,
        check: &'static str,
        detail: &'static str,
    ) {
        if facts.mode & required != required {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                detail,
                "restore the path permissions from a current release bundle",
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
        self.check_requirement(
            path,
            owner_uid,
            Some(TrustedRoot::strict(system_root, owner_uid)),
            InstallerPathPolicy::SystemReadableInput,
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
        let Some(config_directory) = config_path.parent() else {
            self.report.refuse(
                "config-state",
                Some(config_path.to_path_buf()),
                "config path has no parent directory",
                "place config.env in a user-owned configuration directory",
            );
            return;
        };
        let config_trust = TrustedRoot::owned(config_directory, owner_uid);
        self.check_requirement(
            config_directory,
            owner_uid,
            Some(config_trust),
            InstallerPathPolicy::InputDirectory,
            "config-state",
        );
        self.check_requirement(
            config_path,
            owner_uid,
            Some(config_trust),
            InstallerPathPolicy::ReadableInput,
            "config-state",
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
                    PathKind::Directory => {
                        self.check_permissions(
                            &entry,
                            &facts,
                            0o500,
                            "config-state",
                            "config directory is not readable and searchable by its owner",
                        );
                        pending.push(entry);
                    }
                    PathKind::File => self.check_permissions(
                        &entry,
                        &facts,
                        0o400,
                        "config-state",
                        "config file is not readable by its owner",
                    ),
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

    fn check_not_writable_by_others(&mut self, path: &Path, facts: &PathFacts) {
        if facts.mode & 0o022 != 0 {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                "trusted path is writable by its group or by other users",
                "remove group and other write permission from the trusted path",
            );
        }
    }

    fn check_candidate_external_ancestor(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        user_owner_uid: u32,
    ) {
        if facts.owner_uid != 0 && facts.owner_uid != user_owner_uid {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                format!(
                    "candidate path ancestor is owned by uid {}, expected root or uid {user_owner_uid}",
                    facts.owner_uid
                ),
                "move the verified bundle below a root- or user-owned directory",
            );
            return;
        }
        if facts.mode & 0o022 != 0 && facts.mode & 0o1000 == 0 {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                "candidate path ancestor is writable by its group or by other users without sticky-directory protection",
                "move the verified bundle into a private directory or below a sticky shared directory such as /tmp",
            );
        }
    }

    fn check_ancestors(&mut self, path: &Path, trusted_root: Option<TrustedRoot<'_>>) {
        for ancestor in path.ancestors().skip(1) {
            let ancestor_policy = trusted_root.and_then(|root| {
                if ancestor.starts_with(root.path) {
                    Some(AncestorPolicy::Trusted {
                        owner_uid: root.owner_uid,
                        reject_other_writes: root.reject_other_writes,
                    })
                } else if root.protect_external_ancestors {
                    Some(AncestorPolicy::CandidateExternal {
                        user_owner_uid: root.owner_uid,
                    })
                } else {
                    None
                }
            });
            if !self
                .checked_ancestors
                .insert((ancestor.to_path_buf(), ancestor_policy))
            {
                continue;
            }
            match self.filesystem.path_facts(ancestor) {
                Ok(facts) if facts.kind == PathKind::Directory => match ancestor_policy {
                    Some(AncestorPolicy::Trusted {
                        owner_uid,
                        reject_other_writes,
                    }) => {
                        self.check_owner(ancestor, &facts, owner_uid, "path-containment");
                        if reject_other_writes {
                            self.check_not_writable_by_others(ancestor, &facts);
                        }
                    }
                    Some(AncestorPolicy::CandidateExternal { user_owner_uid }) => {
                        self.check_candidate_external_ancestor(ancestor, &facts, user_owner_uid)
                    }
                    None => {}
                },
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
            false,
        );

        assert!(initial.compatible(), "{}", initial.render());
        assert!(candidate.compatible(), "{}", candidate.render());
    }

    #[test]
    fn python_environment_safety_is_required_only_when_repair_is_requested() {
        let fixture = InstalledFixture::new("conditional-python-repair");
        let virtualenv = fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP");
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let missing = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );
        assert!(missing.compatible(), "{}", missing.render());

        fs::create_dir_all(&virtualenv).unwrap();
        let repair = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );
        assert!(repair.compatible(), "{}", repair.render());

        fs::remove_dir(&virtualenv).unwrap();
        symlink(&fixture.config_directory, &virtualenv).unwrap();
        let preserving = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );
        let repair = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert!(preserving.compatible(), "{}", preserving.render());
        assert_failure(&repair, "python-environment-repair", &virtualenv, "Symlink");
    }

    #[test]
    fn existing_user_desktop_launcher_must_be_safely_replaceable() {
        let fixture = InstalledFixture::new("user-desktop-launcher");
        let launcher = fixture.facts.layout.user_desktop_entry();
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        write_file(&launcher, false);
        set_mode(&launcher, 0o400);
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );

        assert_failure(
            &report,
            "user-desktop",
            &launcher,
            "not writable by its owner",
        );
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
    fn installer_path_policy_permission_matrix_is_enforced() {
        let cases = [
            (
                "replace-file",
                InstallerPathPolicy::ReplaceFile,
                0o400,
                "not writable by its owner",
            ),
            (
                "replace-executable",
                InstallerPathPolicy::ReplaceExecutable,
                0o600,
                "no execute permission",
            ),
            (
                "mutate-directory",
                InstallerPathPolicy::MutateDirectory,
                0o500,
                "not writable and searchable",
            ),
            (
                "recursive-clear",
                InstallerPathPolicy::RecursiveClear,
                0o500,
                "not writable and searchable",
            ),
            (
                "exact-drop-in",
                InstallerPathPolicy::ExactDropInDirectory {
                    expected_entry: "config.conf",
                },
                0o500,
                "not readable, writable, and searchable",
            ),
            (
                "readable-input",
                InstallerPathPolicy::ReadableInput,
                0o200,
                "not readable by its owner",
            ),
            (
                "system-readable-input",
                InstallerPathPolicy::SystemReadableInput,
                0o400,
                "invoking user",
            ),
            (
                "executable-input",
                InstallerPathPolicy::ExecutableInput,
                0o400,
                "not readable and executable",
            ),
            (
                "input-directory",
                InstallerPathPolicy::InputDirectory,
                0o400,
                "not readable and searchable",
            ),
        ];

        for (label, policy, mode, expected_detail) in cases {
            let fixture = InstalledFixture::new(label);
            let (path, trusted_root, owner_uid) = match policy {
                InstallerPathPolicy::ReplaceFile => (
                    fixture
                        .facts
                        .layout
                        .system_path("/etc/systemd/system/LG_Buddy.service"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ReplaceExecutable => (
                    fixture.facts.layout.installed_executable(),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::MutateDirectory => (
                    fixture.facts.layout.system_path("/usr/bin"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::RecursiveClear => (
                    fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ExactDropInDirectory { .. } => (
                    fixture
                        .facts
                        .layout
                        .system_path("/etc/systemd/system/LG_Buddy.service.d"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ReadableInput => (
                    fixture.candidate_root.join("release-manifest.json"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
                InstallerPathPolicy::SystemReadableInput => (
                    fixture.facts.layout.config_pointer(),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ExecutableInput => (
                    fixture.candidate_root.join("install.sh"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
                InstallerPathPolicy::InputDirectory => (
                    fixture.candidate_root.join("systemd"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
            };
            if policy == InstallerPathPolicy::RecursiveClear {
                fs::create_dir_all(&path).unwrap();
            }
            let filesystem = OverriddenFilesystem {
                path: path.clone(),
                owner_uid: None,
                mode: Some(mode),
                read_only: None,
                mount_point: None,
            };
            let mut checker = Checker::new(&filesystem);
            checker.check_requirement(
                &path,
                owner_uid,
                Some(TrustedRoot::strict(&trusted_root, owner_uid)),
                policy,
                "policy-contract",
            );

            assert_failure(&checker.report, "policy-contract", &path, expected_detail);
        }
    }

    #[test]
    fn candidate_input_policy_refuses_group_or_other_write_access() {
        for (label, path, mode) in [
            ("writable-manifest", "release-manifest.json", 0o660),
            ("writable-installer", "install.sh", 0o770),
            ("writable-input-directory", "systemd", 0o770),
        ] {
            let fixture = InstalledFixture::new(label);
            let input = fixture.candidate_root.join(path);
            set_mode(&input, mode);

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(
                &report,
                "path-containment",
                &input,
                "writable by its group or by other users",
            );
        }
    }

    #[test]
    fn candidate_preflight_refuses_a_non_sticky_writable_external_ancestor() {
        let fixture = InstalledFixture::new("writable-candidate-ancestor");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        set_mode(&ancestor, 0o777);

        let report = evaluate_candidate_preflight(
            &OsFilesystemFacts,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(
            &report,
            "path-containment",
            &ancestor,
            "without sticky-directory protection",
        );
    }

    #[test]
    fn candidate_preflight_refuses_an_external_ancestor_owned_by_another_user() {
        let fixture = InstalledFixture::new("untrusted-candidate-ancestor-owner");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        let filesystem = OverriddenFilesystem {
            path: ancestor.clone(),
            owner_uid: Some(fixture.facts.user_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_candidate_preflight(
            &filesystem,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(
            &report,
            "path-containment",
            &ancestor,
            "expected root or uid",
        );
    }

    #[test]
    fn candidate_preflight_accepts_a_root_owned_sticky_external_ancestor() {
        let fixture = InstalledFixture::new("sticky-candidate-ancestor");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        let filesystem = OverriddenFilesystem {
            path: ancestor,
            owner_uid: Some(0),
            mode: Some(0o1777),
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_candidate_preflight(
            &filesystem,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert!(report.compatible(), "{}", report.render());
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
    fn python_repair_preflight_refuses_a_symlinked_installed_virtualenv() {
        let fixture = InstalledFixture::new("symlink-virtualenv");
        let virtualenv = fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP");
        let target = fixture.root.join("external-virtualenv");
        fs::create_dir(&virtualenv).unwrap();
        fs::remove_dir(&virtualenv).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&target, &virtualenv).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert_failure(&report, "python-environment-repair", &virtualenv, "Symlink");
    }

    #[test]
    fn python_repair_preflight_refuses_a_nested_virtualenv_mount() {
        let fixture = InstalledFixture::new("nested-virtualenv-mount");
        let nested_mount = fixture
            .facts
            .layout
            .system_path("/usr/bin/LG_Buddy_PIP/lib/python/site-packages");
        fs::create_dir_all(&nested_mount).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");
        let filesystem = OverriddenFilesystem {
            path: nested_mount.clone(),
            owner_uid: None,
            mode: None,
            read_only: None,
            mount_point: Some(true),
        };

        let report = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert_failure(
            &report,
            "python-environment-repair",
            &nested_mount,
            "nested mount point",
        );
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
    fn initial_preflight_refuses_a_missing_exact_systemd_drop_in() {
        let fixture = InstalledFixture::new("missing-systemd-drop-in");
        let drop_in = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf");
        fs::remove_file(&drop_in).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &drop_in,
            "required drop-in entry is missing",
        );
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
        assert_failure(
            &report,
            "candidate-layout",
            &installer,
            "not readable and executable",
        );
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
    fn candidate_preflight_refuses_each_missing_upgrade_input() {
        for (index, requirement) in CANDIDATE_PATH_REQUIREMENTS
            .iter()
            .filter(|requirement| !requirement.path.is_empty())
            .enumerate()
        {
            let fixture = InstalledFixture::new(&format!("missing-candidate-input-{index}"));
            let input = fixture.candidate_root.join(requirement.path);
            if requirement.policy.expects_directory() {
                fs::remove_dir_all(&input).unwrap();
            } else {
                fs::remove_file(&input).unwrap();
            }

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(&report, "candidate-layout", &input, "missing");
        }
    }

    #[test]
    fn candidate_host_preflight_must_run_from_the_verified_bundle() {
        let fixture = InstalledFixture::new("wrong-candidate-runtime");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &fixture.facts,
            &fixture.candidate_root,
            false,
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
            false,
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

        fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
            let mut mount_points = OsFilesystemFacts.mount_points()?;
            match self.mount_point {
                Some(true) if !mount_points.contains(&self.path) => {
                    mount_points.push(self.path.clone());
                }
                Some(false) => mount_points.retain(|path| path != &self.path),
                _ => {}
            }
            Ok(mount_points)
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

            create_system_requirements(&layout);
            set_directory_tree_mode(&system_root, 0o755);
            create_relative_requirements(&layout.user_systemd_path(""), USER_PATH_REQUIREMENTS);
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
            set_directory_tree_mode(&user_home, 0o755);

            let candidate_root = root.join("candidate");
            create_relative_requirements(&candidate_root, CANDIDATE_PATH_REQUIREMENTS);
            set_directory_tree_mode(&candidate_root, 0o755);
            set_mode(&root, 0o755);

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

    fn create_system_requirements(layout: &InstalledLayout) {
        for requirement in SYSTEM_PATH_REQUIREMENTS
            .iter()
            .filter(|requirement| requirement.policy.expects_directory())
        {
            fs::create_dir_all(layout.system_path(requirement.path)).unwrap();
        }
        for requirement in SYSTEM_PATH_REQUIREMENTS
            .iter()
            .filter(|requirement| requirement.policy.expects_file())
        {
            write_file(
                &layout.system_path(requirement.path),
                matches!(requirement.policy, InstallerPathPolicy::ReplaceExecutable),
            );
        }
    }

    fn create_relative_requirements(base: &Path, requirements: &[InstallerPathRequirement]) {
        for requirement in requirements
            .iter()
            .filter(|requirement| requirement.policy.expects_directory())
        {
            fs::create_dir_all(base.join(requirement.path)).unwrap();
        }
        for requirement in requirements
            .iter()
            .filter(|requirement| requirement.policy.expects_file())
        {
            write_file(
                &base.join(requirement.path),
                matches!(requirement.policy, InstallerPathPolicy::ExecutableInput),
            );
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
