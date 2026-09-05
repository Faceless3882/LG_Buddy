use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;
use crate::session::{SessionEvent, SessionObservation};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(25);

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

pub(crate) fn run(
    idle_timeout_secs: u64,
    event_path: &Path,
    mut on_observation: impl FnMut(SessionObservation) -> bool,
) -> Result<(), SwayidleSourceError> {
    let event_file = create_event_file(event_path).map_err(SwayidleSourceError::Io)?;
    let _event_file_guard = EventFileGuard(event_path.to_path_buf());
    let mut reader = BufReader::new(event_file);
    let child = Command::new("swayidle")
        .args(command_args(idle_timeout_secs, event_path))
        .spawn()
        .map_err(SwayidleSourceError::Io)?;
    let mut child = ChildGuard(child);

    loop {
        if !drain_events(&mut reader, &mut on_observation).map_err(SwayidleSourceError::Io)? {
            let _ = child.0.kill();
            let _ = child.0.wait();
            return Ok(());
        }

        if let Some(status) = child.0.try_wait().map_err(SwayidleSourceError::Io)? {
            let _ =
                drain_events(&mut reader, &mut on_observation).map_err(SwayidleSourceError::Io)?;
            return if status.success() {
                Ok(())
            } else {
                Err(SwayidleSourceError::Exited(status))
            };
        }

        thread::sleep(EVENT_POLL_INTERVAL);
    }
}

struct EventFileGuard(PathBuf);

impl Drop for EventFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn create_event_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn drain_events<R: BufRead>(
    reader: &mut R,
    on_observation: &mut impl FnMut(SessionObservation) -> bool,
) -> io::Result<bool> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(true);
        }

        let observed_at = Instant::now();
        let observation = match line.trim() {
            "idle" => SessionObservation::Event {
                event: SessionEvent::Idle,
                source: EventSource::DesktopSession,
                observed_at,
            },
            "active" => SessionObservation::Inactivity {
                observation: InactivityObservation::DesktopActivityObserved,
                source: EventSource::DesktopSession,
                observed_at,
            },
            _ => continue,
        };
        if !on_observation(observation) {
            return Ok(false);
        }
    }
}

fn command_args(idle_timeout_secs: u64, event_path: &Path) -> Vec<String> {
    let path = shell_quote(event_path);
    vec![
        "-w".to_string(),
        "timeout".to_string(),
        idle_timeout_secs.to_string(),
        format!("printf '%s\\n' idle >> {path}"),
        "resume".to_string(),
        format!("printf '%s\\n' active >> {path}"),
    ]
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let escaped = rendered.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::{command_args, drain_events, shell_quote};
    use crate::events::EventSource;
    use crate::session::inactivity::InactivityObservation;
    use crate::session::{SessionEvent, SessionObservation};
    use std::io::Cursor;
    use std::path::Path;

    #[test]
    fn production_invocation_emits_idle_and_active_facts() {
        assert_eq!(
            command_args(300, Path::new("/run/user/1000/LG Buddy/events")),
            vec![
                "-w",
                "timeout",
                "300",
                "printf '%s\\n' idle >> '/run/user/1000/LG Buddy/events'",
                "resume",
                "printf '%s\\n' active >> '/run/user/1000/LG Buddy/events'",
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

    #[test]
    fn resume_is_independent_desktop_activity() {
        let mut input = Cursor::new(b"idle\nactive\n");
        let mut observations = Vec::new();

        assert!(drain_events(&mut input, &mut |observation| {
            observations.push(observation);
            true
        })
        .expect("read swayidle observations"));

        assert!(matches!(
            observations[0],
            SessionObservation::Event {
                event: SessionEvent::Idle,
                source: EventSource::DesktopSession,
                ..
            }
        ));
        assert!(matches!(
            observations[1],
            SessionObservation::Inactivity {
                observation: InactivityObservation::DesktopActivityObserved,
                source: EventSource::DesktopSession,
                ..
            }
        ));
    }
}
