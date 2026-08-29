use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Instant;

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

const REQUIRED_IDLE_NOTIFIER_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaylandProviderCapabilities {
    pub idle_notifier_version: u32,
    pub seat_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaylandProviderError {
    Connection(String),
    Dispatch(String),
    MissingIdleNotifier,
    UnsupportedIdleNotifierVersion(u32),
    NoSeats,
    IdleNotifierRemoved,
    LastSeatRemoved,
}

impl fmt::Display for WaylandProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(message) => {
                write!(f, "failed to connect to the Wayland compositor: {message}")
            }
            Self::Dispatch(message) => {
                write!(f, "Wayland event dispatch failed: {message}")
            }
            Self::MissingIdleNotifier => write!(
                f,
                "the compositor does not advertise ext_idle_notifier_v1; version 2 or newer is required"
            ),
            Self::UnsupportedIdleNotifierVersion(version) => write!(
                f,
                "the compositor advertises ext_idle_notifier_v1 version {version}; version 2 or newer is required"
            ),
            Self::NoSeats => write!(f, "the compositor does not advertise a Wayland seat"),
            Self::IdleNotifierRemoved => write!(
                f,
                "the compositor removed the ext_idle_notifier_v1 global"
            ),
            Self::LastSeatRemoved => {
                write!(f, "the compositor removed the last monitored Wayland seat")
            }
        }
    }
}

impl Error for WaylandProviderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlobalInfo {
    name: u32,
    version: u32,
}

#[derive(Debug, Default)]
struct RegistryFacts {
    idle_notifiers: HashMap<u32, u32>,
    seats: HashMap<u32, u32>,
}

impl RegistryFacts {
    fn add(&mut self, name: u32, interface: &str, version: u32) {
        if interface == ext_idle_notifier_v1::ExtIdleNotifierV1::interface().name {
            self.idle_notifiers.insert(name, version);
        } else if interface == wl_seat::WlSeat::interface().name {
            self.seats.insert(name, version);
        }
    }

    fn remove(&mut self, name: u32) {
        self.idle_notifiers.remove(&name);
        self.seats.remove(&name);
    }

    fn selected_idle_notifier(&self) -> Option<GlobalInfo> {
        self.idle_notifiers
            .iter()
            .filter(|(_, version)| **version >= REQUIRED_IDLE_NOTIFIER_VERSION)
            .max_by_key(|(_, version)| **version)
            .map(|(name, version)| GlobalInfo {
                name: *name,
                version: *version,
            })
    }

    fn maximum_idle_notifier_version(&self) -> Option<u32> {
        self.idle_notifiers.values().copied().max()
    }

    fn capabilities(&self) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
        let Some(version) = self.maximum_idle_notifier_version() else {
            return Err(WaylandProviderError::MissingIdleNotifier);
        };
        if version < REQUIRED_IDLE_NOTIFIER_VERSION {
            return Err(WaylandProviderError::UnsupportedIdleNotifierVersion(
                version,
            ));
        }
        if self.seats.is_empty() {
            return Err(WaylandProviderError::NoSeats);
        }

        Ok(WaylandProviderCapabilities {
            idle_notifier_version: version,
            seat_count: self.seats.len(),
        })
    }
}

struct SeatBinding {
    seat: wl_seat::WlSeat,
    notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
}

struct WaylandProviderState<F> {
    registry: Option<wl_registry::WlRegistry>,
    registry_facts: RegistryFacts,
    idle_notifier: Option<(u32, ext_idle_notifier_v1::ExtIdleNotifierV1)>,
    seats: HashMap<u32, SeatBinding>,
    initialized: bool,
    running: bool,
    error: Option<WaylandProviderError>,
    on_activity: F,
}

impl<F> WaylandProviderState<F>
where
    F: FnMut(Instant) -> bool + 'static,
{
    fn new(on_activity: F) -> Self {
        Self {
            registry: None,
            registry_facts: RegistryFacts::default(),
            idle_notifier: None,
            seats: HashMap::new(),
            initialized: false,
            running: true,
            error: None,
            on_activity,
        }
    }

    fn bind_idle_notifier(
        &mut self,
        registry: &wl_registry::WlRegistry,
        queue_handle: &QueueHandle<Self>,
    ) {
        if self.idle_notifier.is_some() {
            return;
        }
        let Some(global) = self.registry_facts.selected_idle_notifier() else {
            return;
        };

        let notifier = registry.bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(
            global.name,
            REQUIRED_IDLE_NOTIFIER_VERSION.min(global.version),
            queue_handle,
            (),
        );
        self.idle_notifier = Some((global.name, notifier));
        self.attach_unmonitored_seats(queue_handle);
    }

    fn bind_seat(
        &mut self,
        registry: &wl_registry::WlRegistry,
        queue_handle: &QueueHandle<Self>,
        name: u32,
        _version: u32,
    ) {
        if self.seats.contains_key(&name) {
            return;
        }

        let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, 1, queue_handle, name);
        self.seats.insert(
            name,
            SeatBinding {
                seat,
                notification: None,
            },
        );
        self.attach_seat(name, queue_handle);
    }

    fn attach_unmonitored_seats(&mut self, queue_handle: &QueueHandle<Self>) {
        let seat_names: Vec<u32> = self.seats.keys().copied().collect();
        for name in seat_names {
            self.attach_seat(name, queue_handle);
        }
    }

    fn attach_seat(&mut self, name: u32, queue_handle: &QueueHandle<Self>) {
        let Some((_, notifier)) = self.idle_notifier.as_ref() else {
            return;
        };
        let Some(binding) = self.seats.get_mut(&name) else {
            return;
        };
        if binding.notification.is_some() {
            return;
        }

        binding.notification =
            Some(notifier.get_input_idle_notification(0, &binding.seat, queue_handle, name));
    }

    fn remove_global(&mut self, name: u32) {
        self.registry_facts.remove(name);

        let removed_bound_notifier = self
            .idle_notifier
            .as_ref()
            .is_some_and(|(global_name, _)| *global_name == name);

        let removed_seat = if let Some(mut binding) = self.seats.remove(&name) {
            if let Some(notification) = binding.notification.take() {
                notification.destroy();
            }
            true
        } else {
            false
        };

        if let Some(err) = global_removal_error(
            self.initialized,
            removed_bound_notifier,
            removed_seat,
            self.seats.len(),
        ) {
            self.error = Some(err);
            self.running = false;
        }
    }

    fn take_error(&mut self) -> Option<WaylandProviderError> {
        self.error.take()
    }
}

fn global_removal_error(
    initialized: bool,
    removed_bound_notifier: bool,
    removed_seat: bool,
    remaining_seat_count: usize,
) -> Option<WaylandProviderError> {
    if removed_bound_notifier {
        Some(WaylandProviderError::IdleNotifierRemoved)
    } else if initialized && removed_seat && remaining_seat_count == 0 {
        Some(WaylandProviderError::LastSeatRemoved)
    } else {
        None
    }
}

fn notification_is_activity(event: &ext_idle_notification_v1::Event) -> bool {
    matches!(event, ext_idle_notification_v1::Event::Resumed)
}

impl<F> Dispatch<wl_registry::WlRegistry, ()> for WaylandProviderState<F>
where
    F: FnMut(Instant) -> bool + 'static,
{
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.registry_facts.add(name, interface.as_str(), version);
                if interface == ext_idle_notifier_v1::ExtIdleNotifierV1::interface().name {
                    state.bind_idle_notifier(registry, queue_handle);
                } else if interface == wl_seat::WlSeat::interface().name {
                    state.bind_seat(registry, queue_handle, name, version);
                }
            }
            wl_registry::Event::GlobalRemove { name } => state.remove_global(name),
            _ => {}
        }
    }
}

impl<F> Dispatch<wl_seat::WlSeat, u32> for WaylandProviderState<F>
where
    F: FnMut(Instant) -> bool + 'static,
{
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl<F> Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for WaylandProviderState<F>
where
    F: FnMut(Instant) -> bool + 'static,
{
    fn event(
        _: &mut Self,
        _: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl<F> Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, u32> for WaylandProviderState<F>
where
    F: FnMut(Instant) -> bool + 'static,
{
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if notification_is_activity(&event) && !(state.on_activity)(Instant::now()) {
            state.running = false;
        }
    }
}

type InitializedWaylandProvider<F> = (
    EventQueue<WaylandProviderState<F>>,
    WaylandProviderState<F>,
    WaylandProviderCapabilities,
);

fn initialize_provider<F>(
    connection: Connection,
    on_activity: F,
) -> Result<InitializedWaylandProvider<F>, WaylandProviderError>
where
    F: FnMut(Instant) -> bool + 'static,
{
    let display = connection.display();
    let mut event_queue = connection.new_event_queue();
    let queue_handle = event_queue.handle();
    let mut state = WaylandProviderState::new(on_activity);
    state.registry = Some(display.get_registry(&queue_handle, ()));

    event_queue
        .roundtrip(&mut state)
        .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
    let capabilities = state.registry_facts.capabilities()?;
    state.initialized = true;
    event_queue
        .roundtrip(&mut state)
        .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
    if let Some(err) = state.take_error() {
        return Err(err);
    }

    Ok((event_queue, state, capabilities))
}

pub(crate) fn connect_wayland() -> Result<Connection, WaylandProviderError> {
    // `connect_to_env` removes an inherited WAYLAND_SOCKET from the process
    // environment. Monitor startup must call this before spawning any threads.
    Connection::connect_to_env().map_err(|err| WaylandProviderError::Connection(err.to_string()))
}

pub(crate) fn probe_wayland_capabilities(
) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
    let connection = connect_wayland()?;
    let (_, _, capabilities) = initialize_provider(connection, |_| true)?;
    Ok(capabilities)
}

pub(crate) fn run_wayland_activity_monitor<F>(
    connection: Connection,
    on_activity: F,
) -> Result<(), WaylandProviderError>
where
    F: FnMut(Instant) -> bool + 'static,
{
    let (mut event_queue, mut state, _) = initialize_provider(connection, on_activity)?;

    while state.running {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
        if let Some(err) = state.take_error() {
            return Err(err);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ext_idle_notification_v1, global_removal_error, notification_is_activity, RegistryFacts,
        WaylandProviderCapabilities, WaylandProviderError,
    };

    const NOTIFIER: &str = "ext_idle_notifier_v1";
    const SEAT: &str = "wl_seat";

    #[test]
    fn version_two_notifier_and_any_seat_satisfy_the_contract() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 9);

        assert_eq!(
            facts.capabilities(),
            Ok(WaylandProviderCapabilities {
                idle_notifier_version: 2,
                seat_count: 1,
            })
        );
    }

    #[test]
    fn version_one_notifier_is_rejected_precisely() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 1);
        facts.add(11, SEAT, 9);

        assert_eq!(
            facts.capabilities(),
            Err(WaylandProviderError::UnsupportedIdleNotifierVersion(1))
        );
    }

    #[test]
    fn missing_notifier_is_distinct_from_missing_seat() {
        let mut facts = RegistryFacts::default();
        facts.add(11, SEAT, 9);
        assert_eq!(
            facts.capabilities(),
            Err(WaylandProviderError::MissingIdleNotifier)
        );

        facts.add(10, NOTIFIER, 2);
        facts.remove(11);
        assert_eq!(facts.capabilities(), Err(WaylandProviderError::NoSeats));
    }

    #[test]
    fn all_advertised_seats_are_counted_without_capability_filtering() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 1);
        facts.add(12, SEAT, 9);

        assert_eq!(facts.capabilities().unwrap().seat_count, 2);
    }

    #[test]
    fn highest_notifier_version_is_selected() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 1);
        facts.add(20, NOTIFIER, 2);

        assert_eq!(facts.selected_idle_notifier().unwrap().name, 20);
    }

    #[test]
    fn registry_churn_updates_the_advertised_seat_set() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 1);
        facts.add(12, SEAT, 1);
        facts.remove(11);
        assert_eq!(facts.capabilities().unwrap().seat_count, 1);

        facts.add(13, SEAT, 1);
        assert_eq!(facts.capabilities().unwrap().seat_count, 2);
    }

    #[test]
    fn bound_notifier_and_last_seat_removals_are_fatal_after_startup() {
        assert_eq!(
            global_removal_error(true, true, false, 1),
            Some(WaylandProviderError::IdleNotifierRemoved)
        );
        assert_eq!(
            global_removal_error(true, false, true, 0),
            Some(WaylandProviderError::LastSeatRemoved)
        );
        assert_eq!(global_removal_error(true, false, true, 1), None);
        assert_eq!(global_removal_error(false, false, true, 0), None);
    }

    #[test]
    fn only_resumed_notifications_map_to_desktop_activity() {
        assert!(!notification_is_activity(
            &ext_idle_notification_v1::Event::Idled
        ));
        assert!(notification_is_activity(
            &ext_idle_notification_v1::Event::Resumed
        ));
    }
}
