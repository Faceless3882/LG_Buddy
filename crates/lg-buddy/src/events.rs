use crate::session::SessionEvent;
use crate::{Command, PowerCommand, ScreenCommand, StartupMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub source: EventSource,
    pub kind: RuntimeEventKind,
}

impl RuntimeEvent {
    pub const fn new(source: EventSource, kind: RuntimeEventKind) -> Self {
        Self { source, kind }
    }

    pub fn from_command(command: Command) -> Option<Self> {
        RuntimeEventKind::from_command(command).map(|kind| Self::new(EventSource::CliApi, kind))
    }

    pub const fn from_session_event(source: EventSource, event: SessionEvent) -> Self {
        Self::new(source, RuntimeEventKind::from_session_event(event))
    }

    pub const fn from_logind_prepare_for_sleep(preparing: bool) -> Self {
        let kind = if preparing {
            RuntimeEventKind::MachinePreparingForSleep
        } else {
            RuntimeEventKind::MachineResumed
        };

        Self::new(EventSource::LinuxLogind, kind)
    }

    pub const fn network_teardown_imminent(machine_sleep_pending: Option<bool>) -> Self {
        Self::new(
            EventSource::LinuxNetworkManager,
            RuntimeEventKind::NetworkTeardownImminent {
                machine_sleep_pending,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSource {
    CliApi,
    LinuxLogind,
    LinuxNetworkManager,
    LinuxSystemd,
    DesktopSession,
    AuxiliaryInput,
    FuturePlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventKind {
    MachineStartup { mode: StartupMode },
    MachineShutdownRequested,
    MachinePreparingForSleep,
    MachineResumed,
    NetworkTeardownImminent { machine_sleep_pending: Option<bool> },
    SessionIdle,
    SessionActive,
    SessionLocked,
    SessionUnlocked,
    ScreenWakeRequested,
    UserActivityObserved,
    ScreenBlankRequested,
    ScreenRestoreRequested,
    BrightnessRequested,
    VolumeRequested,
}

impl RuntimeEventKind {
    pub const fn from_session_event(event: SessionEvent) -> Self {
        match event {
            SessionEvent::Idle => Self::SessionIdle,
            SessionEvent::Active => Self::SessionActive,
            SessionEvent::WakeRequested => Self::ScreenWakeRequested,
            SessionEvent::BeforeSleep => Self::MachinePreparingForSleep,
            SessionEvent::AfterResume => Self::MachineResumed,
            SessionEvent::Lock => Self::SessionLocked,
            SessionEvent::Unlock => Self::SessionUnlocked,
            SessionEvent::UserActivity => Self::UserActivityObserved,
        }
    }

    pub fn from_command(command: Command) -> Option<Self> {
        match command {
            Command::Startup(mode) => Some(Self::MachineStartup { mode }),
            Command::Shutdown => Some(Self::MachineShutdownRequested),
            Command::Power(PowerCommand::On) => Some(Self::MachineStartup {
                mode: StartupMode::Boot,
            }),
            Command::Power(PowerCommand::Off) => Some(Self::MachineShutdownRequested),
            Command::SleepPre => Some(Self::MachinePreparingForSleep),
            Command::Sleep | Command::NetworkManagerPreDown => {
                Some(Self::NetworkTeardownImminent {
                    machine_sleep_pending: None,
                })
            }
            Command::Brightness(_) => Some(Self::BrightnessRequested),
            Command::Volume(_) => Some(Self::VolumeRequested),
            Command::Screen(ScreenCommand::Off) => Some(Self::ScreenBlankRequested),
            Command::Screen(ScreenCommand::On) => Some(Self::ScreenRestoreRequested),
            Command::ScreenOff => Some(Self::ScreenBlankRequested),
            Command::ScreenOn => Some(Self::ScreenRestoreRequested),
            Command::Monitor
            | Command::Lifecycle
            | Command::DetectBackend
            | Command::Dev(_)
            | Command::Settings(_)
            | Command::Updates(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::session::SessionEvent;
    use crate::{Command, PowerCommand, ScreenCommand, StartupMode};

    #[test]
    fn cli_commands_map_to_canonical_runtime_events() {
        assert_eq!(
            RuntimeEvent::from_command(Command::ScreenOff),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::ScreenBlankRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::ScreenOn),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::ScreenRestoreRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Screen(ScreenCommand::Off)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::ScreenBlankRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Screen(ScreenCommand::On)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::ScreenRestoreRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Startup(StartupMode::Wake)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineStartup {
                    mode: StartupMode::Wake
                }
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Shutdown),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineShutdownRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Power(PowerCommand::On)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineStartup {
                    mode: StartupMode::Boot
                }
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Power(PowerCommand::Off)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineShutdownRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Brightness(crate::BrightnessCommand::Prompt)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::BrightnessRequested
            ))
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Volume(crate::VolumeCommand::Get)),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::VolumeRequested
            ))
        );
    }

    #[test]
    fn legacy_sleep_commands_map_without_claiming_sleep_phase() {
        let expected = Some(RuntimeEvent::new(
            EventSource::CliApi,
            RuntimeEventKind::NetworkTeardownImminent {
                machine_sleep_pending: None,
            },
        ));

        assert_eq!(RuntimeEvent::from_command(Command::Sleep), expected);
        assert_eq!(
            RuntimeEvent::from_command(Command::NetworkManagerPreDown),
            expected
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::SleepPre),
            Some(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachinePreparingForSleep
            ))
        );
    }

    #[test]
    fn source_loop_and_diagnostic_commands_are_not_policy_events() {
        assert_eq!(RuntimeEvent::from_command(Command::Monitor), None);
        assert_eq!(RuntimeEvent::from_command(Command::Lifecycle), None);
        assert_eq!(RuntimeEvent::from_command(Command::DetectBackend), None);
        assert_eq!(
            RuntimeEvent::from_command(Command::Dev(crate::DevCommand::WebOsAuthProbe)),
            None
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Settings(crate::settings::SettingsCommand::List)),
            None
        );
        assert_eq!(
            RuntimeEvent::from_command(Command::Updates(crate::updates::UpdatesCommand::Check {
                notify: false,
            })),
            None
        );
    }

    #[test]
    fn session_events_map_to_canonical_kinds_and_preserve_source() {
        assert_eq!(
            RuntimeEvent::from_session_event(EventSource::DesktopSession, SessionEvent::Idle),
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionIdle)
        );
        assert_eq!(
            RuntimeEvent::from_session_event(EventSource::DesktopSession, SessionEvent::Active),
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionActive)
        );
        assert_eq!(
            RuntimeEvent::from_session_event(
                EventSource::DesktopSession,
                SessionEvent::WakeRequested,
            ),
            RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::ScreenWakeRequested,
            )
        );
        assert_eq!(
            RuntimeEvent::from_session_event(
                EventSource::AuxiliaryInput,
                SessionEvent::UserActivity
            ),
            RuntimeEvent::new(
                EventSource::AuxiliaryInput,
                RuntimeEventKind::UserActivityObserved,
            )
        );
    }

    #[test]
    fn lock_and_unlock_events_are_canonical_session_facts() {
        assert_eq!(
            RuntimeEvent::from_session_event(EventSource::DesktopSession, SessionEvent::Lock),
            RuntimeEvent::new(EventSource::DesktopSession, RuntimeEventKind::SessionLocked)
        );
        assert_eq!(
            RuntimeEvent::from_session_event(EventSource::DesktopSession, SessionEvent::Unlock),
            RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::SessionUnlocked
            )
        );
    }

    #[test]
    fn logind_prepare_for_sleep_maps_to_machine_lifecycle_facts() {
        assert_eq!(
            RuntimeEvent::from_logind_prepare_for_sleep(true),
            RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::MachinePreparingForSleep,
            )
        );
        assert_eq!(
            RuntimeEvent::from_logind_prepare_for_sleep(false),
            RuntimeEvent::new(EventSource::LinuxLogind, RuntimeEventKind::MachineResumed)
        );
    }

    #[test]
    fn network_manager_event_preserves_sleep_phase_reading() {
        assert_eq!(
            RuntimeEvent::network_teardown_imminent(Some(true)),
            RuntimeEvent::new(
                EventSource::LinuxNetworkManager,
                RuntimeEventKind::NetworkTeardownImminent {
                    machine_sleep_pending: Some(true),
                },
            )
        );
        assert_eq!(
            RuntimeEvent::network_teardown_imminent(Some(false)),
            RuntimeEvent::new(
                EventSource::LinuxNetworkManager,
                RuntimeEventKind::NetworkTeardownImminent {
                    machine_sleep_pending: Some(false),
                },
            )
        );
        assert_eq!(
            RuntimeEvent::network_teardown_imminent(None),
            RuntimeEvent::new(
                EventSource::LinuxNetworkManager,
                RuntimeEventKind::NetworkTeardownImminent {
                    machine_sleep_pending: None,
                },
            )
        );
    }
}
