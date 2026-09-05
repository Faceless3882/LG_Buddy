use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

#[derive(Debug)]
pub(crate) enum SwayidleSourceError {
    Io(io::Error),
    Exited(ExitStatus),
}

impl fmt::Display for SwayidleSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Exited(status) => write!(f, "swayidle exited with status {status}"),
        }
    }
}

impl std::error::Error for SwayidleSourceError {}

pub(crate) fn run(idle_timeout_secs: u64, current_exe: &Path) -> Result<(), SwayidleSourceError> {
    let status = Command::new("swayidle")
        .args(command_args(idle_timeout_secs, current_exe))
        .status()
        .map_err(SwayidleSourceError::Io)?;

    if status.success() {
        Ok(())
    } else {
        Err(SwayidleSourceError::Exited(status))
    }
}

fn command_args(idle_timeout_secs: u64, current_exe: &Path) -> Vec<String> {
    let executable = shell_quote(current_exe);
    vec![
        "-w".to_string(),
        "timeout".to_string(),
        idle_timeout_secs.to_string(),
        format!("{executable} screen off"),
        "resume".to_string(),
        format!("{executable} screen on"),
    ]
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let escaped = rendered.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::{command_args, shell_quote};
    use std::path::Path;

    #[test]
    fn production_invocation_uses_only_timeout_and_resume_commands() {
        assert_eq!(
            command_args(300, Path::new("/opt/LG Buddy/lg-buddy")),
            vec![
                "-w",
                "timeout",
                "300",
                "'/opt/LG Buddy/lg-buddy' screen off",
                "resume",
                "'/opt/LG Buddy/lg-buddy' screen on",
            ]
        );
    }

    #[test]
    fn shell_quote_escapes_posix_paths() {
        assert_eq!(shell_quote(Path::new("/tmp/lg buddy")), "'/tmp/lg buddy'");
        assert_eq!(
            shell_quote(Path::new("/tmp/that'one")),
            "'/tmp/that'\"'\"'one'"
        );
    }
}
