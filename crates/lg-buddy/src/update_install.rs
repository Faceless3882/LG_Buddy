use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::Version;

use crate::release_bundle::{
    acquire_release_bundle, resolve_release_identity, verify_release_binary_identity,
    BundleAcquisitionError, ReleaseIdentity, VerifiedReleaseBundle,
};
use crate::updates::{discover_install_candidate, ReleaseInfo, UpdatesError};
use crate::upgrade_preflight::{current_host_preflight, CompatibilityReport};
use crate::version::VersionInfo;

#[derive(Debug)]
pub enum UpdateInstallError {
    Updates(UpdatesError),
    Bundle(BundleAcquisitionError),
    InvalidCurrentVersion {
        version: String,
        source: semver::Error,
    },
    InitialPreflight(CompatibilityReport),
    DowngradeRefused {
        current: Version,
        candidate: Version,
    },
    Output(io::Error),
    ConfirmationRequiresTerminal,
    ConfirmationIo(io::Error),
    TargetChanged {
        confirmed: Box<ReleaseIdentity>,
        acquired: Box<ReleaseIdentity>,
    },
    CandidatePreflightLaunch(io::Error),
    CandidatePreflightFailed(Option<i32>),
    InstallerLaunch(io::Error),
    InstallerFailed(Option<i32>),
    InstalledIdentity(BundleAcquisitionError),
    InstalledIdentityMismatch {
        expected: Box<ReleaseIdentity>,
        observed: Box<ReleaseIdentity>,
    },
}

impl fmt::Display for UpdateInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Updates(error) => write!(formatter, "update discovery failed: {error}"),
            Self::Bundle(error) => write!(formatter, "release bundle verification failed: {error}"),
            Self::InvalidCurrentVersion { version, source } => {
                write!(formatter, "installed version `{version}` is invalid: {source}")
            }
            Self::InitialPreflight(report) => formatter.write_str(&report.render()),
            Self::DowngradeRefused { current, candidate } => write!(
                formatter,
                "refusing to downgrade LG Buddy from {current} to {candidate}"
            ),
            Self::Output(error) => write!(formatter, "could not write upgrade output: {error}"),
            Self::ConfirmationRequiresTerminal => write!(
                formatter,
                "upgrade confirmation requires an interactive terminal"
            ),
            Self::ConfirmationIo(error) => {
                write!(formatter, "could not read upgrade confirmation: {error}")
            }
            Self::TargetChanged {
                confirmed,
                acquired,
            } => write!(
                formatter,
                "verified release identity changed after confirmation (confirmed {confirmed:?}, acquired {acquired:?})"
            ),
            Self::CandidatePreflightLaunch(error) => {
                write!(formatter, "could not run candidate upgrade preflight: {error}")
            }
            Self::CandidatePreflightFailed(code) => write!(
                formatter,
                "candidate upgrade preflight refused the host{}",
                render_exit_code(*code)
            ),
            Self::InstallerLaunch(error) => {
                write!(formatter, "could not start the verified upgrade installer: {error}")
            }
            Self::InstallerFailed(code) => write!(
                formatter,
                "verified upgrade installer failed{}",
                render_exit_code(*code)
            ),
            Self::InstalledIdentity(error) => {
                write!(formatter, "installed release identity verification failed: {error}")
            }
            Self::InstalledIdentityMismatch { expected, observed } => write!(
                formatter,
                "installed release identity {observed:?} does not match verified release identity {expected:?}"
            ),
        }
    }
}

impl Error for UpdateInstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Updates(error) => Some(error),
            Self::Bundle(error) => Some(error),
            Self::InvalidCurrentVersion { source, .. } => Some(source),
            Self::Output(error)
            | Self::ConfirmationIo(error)
            | Self::CandidatePreflightLaunch(error)
            | Self::InstallerLaunch(error) => Some(error),
            Self::InstalledIdentity(error) => Some(error),
            Self::InitialPreflight(_)
            | Self::DowngradeRefused { .. }
            | Self::ConfirmationRequiresTerminal
            | Self::TargetChanged { .. }
            | Self::CandidatePreflightFailed(_)
            | Self::InstallerFailed(_)
            | Self::InstalledIdentityMismatch { .. } => None,
        }
    }
}

fn render_exit_code(code: Option<i32>) -> String {
    code.map(|code| format!(" with exit status {code}"))
        .unwrap_or_else(|| " after termination by signal".to_string())
}

trait BundleView {
    fn identity(&self) -> &ReleaseIdentity;
}

impl BundleView for VerifiedReleaseBundle {
    fn identity(&self) -> &ReleaseIdentity {
        self.identity()
    }
}

trait UpdateInstallRuntime {
    type Bundle: BundleView;

    fn current_version(&mut self) -> VersionInfo;
    fn initial_preflight(&mut self) -> Result<(), UpdateInstallError>;
    fn discover_candidate(
        &mut self,
        current: VersionInfo,
    ) -> Result<ReleaseInfo, UpdateInstallError>;
    fn resolve_target(
        &mut self,
        release: &ReleaseInfo,
    ) -> Result<ReleaseIdentity, UpdateInstallError>;
    fn confirm(&mut self) -> Result<bool, UpdateInstallError>;
    fn acquire(&mut self, release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError>;
    fn candidate_preflight(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError>;
    fn run_installer(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError>;
    fn installed_identity(
        &mut self,
        expected: &ReleaseIdentity,
    ) -> Result<ReleaseIdentity, UpdateInstallError>;
}

struct SystemUpdateInstallRuntime;

impl UpdateInstallRuntime for SystemUpdateInstallRuntime {
    type Bundle = VerifiedReleaseBundle;

    fn current_version(&mut self) -> VersionInfo {
        VersionInfo::current()
    }

    fn initial_preflight(&mut self) -> Result<(), UpdateInstallError> {
        let report = current_host_preflight();
        if report.compatible() {
            Ok(())
        } else {
            Err(UpdateInstallError::InitialPreflight(report))
        }
    }

    fn discover_candidate(
        &mut self,
        current: VersionInfo,
    ) -> Result<ReleaseInfo, UpdateInstallError> {
        discover_install_candidate(current).map_err(UpdateInstallError::Updates)
    }

    fn resolve_target(
        &mut self,
        release: &ReleaseInfo,
    ) -> Result<ReleaseIdentity, UpdateInstallError> {
        resolve_release_identity(release).map_err(UpdateInstallError::Bundle)
    }

    fn confirm(&mut self) -> Result<bool, UpdateInstallError> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(UpdateInstallError::ConfirmationRequiresTerminal);
        }
        let mut answer = String::new();
        io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(UpdateInstallError::ConfirmationIo)?;
        Ok(confirmation_is_yes(&answer))
    }

    fn acquire(&mut self, release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError> {
        acquire_release_bundle(release).map_err(UpdateInstallError::Bundle)
    }

    fn candidate_preflight(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
        let status = candidate_preflight_command(bundle.root())
            .status()
            .map_err(UpdateInstallError::CandidatePreflightLaunch)?;
        if status.success() {
            Ok(())
        } else {
            Err(UpdateInstallError::CandidatePreflightFailed(status.code()))
        }
    }

    fn run_installer(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
        let status = installer_command(bundle.root())
            .status()
            .map_err(UpdateInstallError::InstallerLaunch)?;
        if status.success() {
            Ok(())
        } else {
            Err(UpdateInstallError::InstallerFailed(status.code()))
        }
    }

    fn installed_identity(
        &mut self,
        expected: &ReleaseIdentity,
    ) -> Result<ReleaseIdentity, UpdateInstallError> {
        verify_release_binary_identity(&installed_binary_path(), expected)
            .map_err(UpdateInstallError::InstalledIdentity)
    }
}

fn candidate_preflight_command(candidate_root: &Path) -> Command {
    let mut command = Command::new(candidate_root.join("lg-buddy"));
    command.arg("upgrade-preflight").arg(candidate_root);
    command
}

fn installer_command(candidate_root: &Path) -> Command {
    let mut command = Command::new(candidate_root.join("install.sh"));
    command.arg("--upgrade");
    command
}

fn confirmation_is_yes(answer: &str) -> bool {
    answer.trim() == "yes"
}

fn installed_binary_path() -> PathBuf {
    let install_root = std::env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    installed_binary_path_for_root(install_root)
}

fn installed_binary_path_for_root(install_root: Option<PathBuf>) -> PathBuf {
    install_root
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("usr/bin/lg-buddy")
}

pub fn run_update_install<W: Write>(writer: &mut W) -> Result<(), UpdateInstallError> {
    run_update_install_with(writer, &mut SystemUpdateInstallRuntime)
}

fn run_update_install_with<W: Write, R: UpdateInstallRuntime>(
    writer: &mut W,
    runtime: &mut R,
) -> Result<(), UpdateInstallError> {
    let current = runtime.current_version();
    runtime.initial_preflight()?;
    let current_version = Version::parse(current.version()).map_err(|source| {
        UpdateInstallError::InvalidCurrentVersion {
            version: current.version().to_string(),
            source,
        }
    })?;
    let release = runtime.discover_candidate(current)?;

    match release.version().cmp(&current_version) {
        std::cmp::Ordering::Equal => {
            writeln!(
                writer,
                "LG Buddy {} ({}) is already up to date.",
                current.version(),
                current.channel().as_str()
            )
            .map_err(UpdateInstallError::Output)?;
            return Ok(());
        }
        std::cmp::Ordering::Less => {
            return Err(UpdateInstallError::DowngradeRefused {
                current: current_version,
                candidate: release.version().clone(),
            });
        }
        std::cmp::Ordering::Greater => {}
    }

    let confirmed_target = runtime.resolve_target(&release)?;
    writeln!(
        writer,
        "Current: {} ({}, commit {})",
        current.version(),
        current.channel().as_str(),
        current.commit().unwrap_or("unknown")
    )
    .map_err(UpdateInstallError::Output)?;
    writeln!(
        writer,
        "Target: {} ({}, commit {})",
        confirmed_target.version(),
        confirmed_target.channel().as_str(),
        confirmed_target.commit()
    )
    .map_err(UpdateInstallError::Output)?;
    writeln!(writer, "Release: {}", release.url()).map_err(UpdateInstallError::Output)?;
    write!(writer, "Type `yes` to install this update: ").map_err(UpdateInstallError::Output)?;
    writer.flush().map_err(UpdateInstallError::Output)?;

    if !runtime.confirm()? {
        writeln!(writer, "Upgrade cancelled.").map_err(UpdateInstallError::Output)?;
        return Ok(());
    }

    let bundle = runtime.acquire(&release)?;
    if bundle.identity() != &confirmed_target {
        return Err(UpdateInstallError::TargetChanged {
            confirmed: Box::new(confirmed_target),
            acquired: Box::new(bundle.identity().clone()),
        });
    }
    runtime.candidate_preflight(&bundle)?;
    runtime.run_installer(&bundle)?;
    let installed = runtime.installed_identity(bundle.identity())?;
    if &installed != bundle.identity() {
        return Err(UpdateInstallError::InstalledIdentityMismatch {
            expected: Box::new(bundle.identity().clone()),
            observed: Box::new(installed),
        });
    }
    writeln!(
        writer,
        "Installed: {} ({}, commit {})",
        installed.version(),
        installed.channel().as_str(),
        installed.commit()
    )
    .map_err(UpdateInstallError::Output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_preflight_command, confirmation_is_yes, installed_binary_path_for_root,
        installer_command, run_update_install_with, BundleView, UpdateInstallError,
        UpdateInstallRuntime,
    };
    use crate::release_bundle::{BundleAcquisitionError, ReleaseIdentity};
    use crate::updates::{ReleaseInfo, UpdateChannel};
    use crate::version::{ReleaseChannel, VersionInfo};
    use semver::Version;
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Failure {
        Initial,
        Resolve,
        Confirmation,
        Acquire,
        CandidatePreflight,
        Installer,
        InstalledIdentity,
    }

    struct FakeBundle {
        identity: ReleaseIdentity,
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl BundleView for FakeBundle {
        fn identity(&self) -> &ReleaseIdentity {
            &self.identity
        }
    }

    impl Drop for FakeBundle {
        fn drop(&mut self) {
            self.events.borrow_mut().push("drop_bundle");
        }
    }

    struct FakeRuntime {
        events: Rc<RefCell<Vec<&'static str>>>,
        current_version: &'static str,
        candidate_version: &'static str,
        confirmed: bool,
        failure: Option<Failure>,
        resolved_identity: ReleaseIdentity,
        acquired_identity: ReleaseIdentity,
        installed_identity: ReleaseIdentity,
    }

    impl FakeRuntime {
        fn new(candidate_version: &'static str) -> Self {
            let events = Rc::new(RefCell::new(Vec::new()));
            let identity = identity(candidate_version, "target-commit");
            Self {
                events,
                current_version: "1.4.0",
                candidate_version,
                confirmed: true,
                failure: None,
                resolved_identity: identity.clone(),
                acquired_identity: identity.clone(),
                installed_identity: identity,
            }
        }

        fn event_names(&self) -> Vec<&'static str> {
            self.events.borrow().clone()
        }
    }

    impl UpdateInstallRuntime for FakeRuntime {
        type Bundle = FakeBundle;

        fn current_version(&mut self) -> VersionInfo {
            self.events.borrow_mut().push("current");
            VersionInfo::for_testing(
                self.current_version,
                ReleaseChannel::Stable,
                Some("current-commit"),
            )
        }

        fn initial_preflight(&mut self) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("initial_preflight");
            if self.failure == Some(Failure::Initial) {
                Err(UpdateInstallError::ConfirmationRequiresTerminal)
            } else {
                Ok(())
            }
        }

        fn discover_candidate(
            &mut self,
            _current: VersionInfo,
        ) -> Result<ReleaseInfo, UpdateInstallError> {
            self.events.borrow_mut().push("discover");
            Ok(ReleaseInfo::from_github(
                Version::parse(self.candidate_version).unwrap(),
                UpdateChannel::Stable,
                format!("https://example.test/releases/v{}", self.candidate_version),
                format!("v{}", self.candidate_version),
                Vec::new(),
            ))
        }

        fn resolve_target(
            &mut self,
            _release: &ReleaseInfo,
        ) -> Result<ReleaseIdentity, UpdateInstallError> {
            self.events.borrow_mut().push("resolve");
            if self.failure == Some(Failure::Resolve) {
                Err(bundle_error())
            } else {
                Ok(self.resolved_identity.clone())
            }
        }

        fn confirm(&mut self) -> Result<bool, UpdateInstallError> {
            self.events.borrow_mut().push("confirm");
            if self.failure == Some(Failure::Confirmation) {
                Err(UpdateInstallError::ConfirmationRequiresTerminal)
            } else {
                Ok(self.confirmed)
            }
        }

        fn acquire(&mut self, _release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError> {
            self.events.borrow_mut().push("acquire");
            if self.failure == Some(Failure::Acquire) {
                Err(UpdateInstallError::Bundle(
                    BundleAcquisitionError::ConcurrentAcquisition,
                ))
            } else {
                Ok(FakeBundle {
                    identity: self.acquired_identity.clone(),
                    events: Rc::clone(&self.events),
                })
            }
        }

        fn candidate_preflight(
            &mut self,
            _bundle: &Self::Bundle,
        ) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("candidate_preflight");
            if self.failure == Some(Failure::CandidatePreflight) {
                Err(UpdateInstallError::CandidatePreflightFailed(Some(1)))
            } else {
                Ok(())
            }
        }

        fn run_installer(&mut self, _bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("installer");
            if self.failure == Some(Failure::Installer) {
                Err(UpdateInstallError::InstallerFailed(Some(1)))
            } else {
                Ok(())
            }
        }

        fn installed_identity(
            &mut self,
            _expected: &ReleaseIdentity,
        ) -> Result<ReleaseIdentity, UpdateInstallError> {
            self.events.borrow_mut().push("installed_identity");
            if self.failure == Some(Failure::InstalledIdentity) {
                Err(UpdateInstallError::InstalledIdentity(
                    BundleAcquisitionError::Binary("mismatch".to_string()),
                ))
            } else {
                Ok(self.installed_identity.clone())
            }
        }
    }

    fn identity(version: &str, commit: &str) -> ReleaseIdentity {
        ReleaseIdentity::from_parts(
            format!("v{version}"),
            Version::parse(version).unwrap(),
            UpdateChannel::Stable,
            "x86_64-unknown-linux-musl",
            commit,
        )
    }

    fn bundle_error() -> UpdateInstallError {
        UpdateInstallError::Bundle(BundleAcquisitionError::ReleaseMetadata(
            "test failure".to_string(),
        ))
    }

    #[test]
    fn candidate_and_installer_processes_use_exact_argv_without_a_shell() {
        let root = Path::new("/tmp/release bundle; touch escaped");
        let candidate = candidate_preflight_command(root);
        assert_eq!(
            candidate.get_program(),
            OsStr::new("/tmp/release bundle; touch escaped/lg-buddy")
        );
        assert_eq!(
            candidate.get_args().collect::<Vec<_>>(),
            [OsStr::new("upgrade-preflight"), root.as_os_str()]
        );

        let installer = installer_command(root);
        assert_eq!(
            installer.get_program(),
            OsStr::new("/tmp/release bundle; touch escaped/install.sh")
        );
        assert_eq!(
            installer.get_args().collect::<Vec<_>>(),
            [OsStr::new("--upgrade")]
        );
    }

    #[test]
    fn confirmation_accepts_only_an_explicit_lowercase_yes() {
        assert!(confirmation_is_yes("yes\n"));
        for answer in ["", "y", "YES", "yes please", "no"] {
            assert!(!confirmation_is_yes(answer), "accepted `{answer}`");
        }
    }

    #[test]
    fn installed_binary_path_is_absolute_without_an_install_root_override() {
        assert_eq!(
            installed_binary_path_for_root(None),
            Path::new("/usr/bin/lg-buddy")
        );
        assert_eq!(
            installed_binary_path_for_root(Some(PathBuf::from("/tmp/install-root"))),
            Path::new("/tmp/install-root/usr/bin/lg-buddy")
        );
    }

    #[test]
    fn initial_refusal_stops_before_discovery() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Initial);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(runtime.event_names(), ["current", "initial_preflight"]);
    }

    #[test]
    fn invalid_current_version_stops_before_discovery() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.current_version = "invalid";

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::InvalidCurrentVersion { .. }
        ));
        assert_eq!(runtime.event_names(), ["current", "initial_preflight"]);
    }

    #[test]
    fn equal_version_stops_before_resolution_and_confirmation() {
        let mut runtime = FakeRuntime::new("1.4.0");
        let mut output = Vec::new();

        run_update_install_with(&mut output, &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover"]
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("already up to date"));
    }

    #[test]
    fn downgrade_stops_before_resolution_and_confirmation() {
        let mut runtime = FakeRuntime::new("1.3.0");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(error, UpdateInstallError::DowngradeRefused { .. }));
        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover"]
        );
    }

    #[test]
    fn declined_confirmation_does_not_acquire_or_mutate() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.confirmed = false;

        run_update_install_with(&mut Vec::new(), &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm"
            ]
        );
    }

    #[test]
    fn unavailable_terminal_stops_before_acquisition() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Confirmation);

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::ConfirmationRequiresTerminal
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm"
            ]
        );
    }

    #[test]
    fn acquisition_failure_does_not_run_candidate_or_installer() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Acquire);

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::Bundle(BundleAcquisitionError::ConcurrentAcquisition)
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire"
            ]
        );
    }

    #[test]
    fn changed_identity_stops_before_candidate_preflight() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.acquired_identity = identity("1.5.0", "different-commit");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(error, UpdateInstallError::TargetChanged { .. }));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn candidate_refusal_stops_before_installer() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::CandidatePreflight);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installer_failure_skips_post_install_verification() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Installer);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installed_identity_failure_is_reported_before_bundle_cleanup() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::InstalledIdentity);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installed_identity_mismatch_is_rejected_before_bundle_cleanup() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.installed_identity = identity("1.5.0", "wrong-commit");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::InstalledIdentityMismatch { .. }
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn successful_upgrade_preserves_order_and_reports_identities() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let mut output = Vec::new();

        run_update_install_with(&mut output, &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Current: 1.4.0 (stable, commit current-commit)"));
        assert!(output.contains("Target: 1.5.0 (stable, commit target-commit)"));
        assert!(output.contains("Installed: 1.5.0 (stable, commit target-commit)"));
    }

    #[test]
    fn resolver_failure_stops_before_confirmation() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Resolve);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover", "resolve"]
        );
    }
}
