use std::fmt;
use std::os::fd::OwnedFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::events::{EventSource, RuntimeEvent};
use crate::session::{SessionEvent, SessionObservation};
use crate::session_bus::{
    get_name_owner, new_system_bus_client, parse_name_owner_changed_signal, BusMethodCall,
    BusSignal, BusSignalMatch, BusValue, SessionBusClient, SessionBusError, DBUS_INTERFACE,
    DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};

pub const LOGIND_SERVICE_NAME: &str = "org.freedesktop.login1";
pub const LOGIND_MANAGER_PATH: &str = "/org/freedesktop/login1";
pub const LOGIND_MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub const LOGIND_SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
pub const LOGIND_INHIBIT_WHO: &str = "LG Buddy";
pub const LOGIND_INHIBIT_WHY: &str = "Handle LG TV power state around system sleep";
const DBUS_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const GRAPHICAL_SESSION_TYPES: [&str; 2] = ["wayland", "x11"];
const USER_SESSION_CLASSES: [&str; 3] = ["user", "user-early", "user-light"];
const LOGIND_LOCK_PROCESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogindSession {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogindSessionError {
    Bus(SessionBusError),
    MalformedReply(&'static str),
    NoCurrentGraphicalSession,
    AmbiguousGraphicalSessions(Vec<String>),
}

impl fmt::Display for LogindSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(err) => write!(f, "{err}"),
            Self::MalformedReply(expected) => {
                write!(f, "malformed logind reply: expected {expected}")
            }
            Self::NoCurrentGraphicalSession => {
                write!(
                    f,
                    "no active local graphical logind session belongs to this user"
                )
            }
            Self::AmbiguousGraphicalSessions(ids) => write!(
                f,
                "multiple active local graphical logind sessions belong to this user: {}",
                ids.join(", ")
            ),
        }
    }
}

impl std::error::Error for LogindSessionError {}

impl From<SessionBusError> for LogindSessionError {
    fn from(value: SessionBusError) -> Self {
        Self::Bus(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedHintChange {
    Changed(bool),
    Invalidated,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LockedHintTracker {
    locked: Option<bool>,
}

impl LockedHintTracker {
    pub fn observe(&mut self, locked: bool) -> Option<SessionEvent> {
        if self.locked == Some(locked) {
            return None;
        }

        let previous = self.locked.replace(locked);
        match (previous, locked) {
            (None | Some(false), true) => Some(crate::session::SessionEvent::Lock),
            (Some(true), false) => Some(crate::session::SessionEvent::Unlock),
            (None | Some(false), false) => None,
            (Some(true), true) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct LogindLockObserver {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LogindLockObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn spawn_lock_observer<F, E>(mut publish: F, report_error: E) -> LogindLockObserver
where
    F: FnMut(SessionObservation) -> bool + Send + 'static,
    E: FnOnce(LogindSessionError) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let result = new_system_bus_client()
            .map_err(Into::into)
            .and_then(|mut bus| run_lock_observer_process(&mut bus, &mut publish, &thread_stop));
        if let Err(err) = result {
            report_error(err);
        }
    });

    LogindLockObserver {
        stop,
        handle: Some(handle),
    }
}

fn run_lock_observer_process<F>(
    bus: &mut impl SessionBusClient,
    publish: &mut F,
    stop: &AtomicBool,
) -> Result<(), LogindSessionError>
where
    F: FnMut(SessionObservation) -> bool,
{
    let explicit_session_id = std::env::var("XDG_SESSION_ID").ok();
    let current_uid = current_effective_uid();
    run_lock_observer_for_session(
        bus,
        publish,
        stop,
        explicit_session_id.as_deref(),
        current_uid,
    )
}

fn run_lock_observer_for_session<F>(
    bus: &mut impl SessionBusClient,
    publish: &mut F,
    stop: &AtomicBool,
    explicit_session_id: Option<&str>,
    current_uid: u32,
) -> Result<(), LogindSessionError>
where
    F: FnMut(SessionObservation) -> bool,
{
    add_logind_owner_signal_match(bus)?;
    let (mut session, owner, initial_locked) =
        bind_lock_target(bus, explicit_session_id, current_uid)?;
    let mut logind_owner = Some(owner);

    let mut tracker = LockedHintTracker::default();
    if !publish_locked_hint(publish, &mut tracker, initial_locked) {
        return Ok(());
    }

    while !stop.load(Ordering::SeqCst) {
        let Some(signal) = bus.process(LOGIND_LOCK_PROCESS_INTERVAL)? else {
            continue;
        };

        if let Some(new_owner) = logind_owner_changed(&signal) {
            if new_owner.is_none() {
                logind_owner = None;
                continue;
            }

            let (new_session, owner, locked) =
                bind_lock_target(bus, explicit_session_id, current_uid)?;
            session = new_session;
            logind_owner = Some(owner);
            if !publish_locked_hint(publish, &mut tracker, locked) {
                return Ok(());
            }
            continue;
        }

        let Some(logind_owner) = logind_owner.as_deref() else {
            continue;
        };
        let Some(change) = map_locked_hint_change(&signal, &session, logind_owner) else {
            continue;
        };
        let locked = match change {
            LockedHintChange::Changed(locked) => locked,
            LockedHintChange::Invalidated => locked_hint(bus, &session)?,
        };
        if !publish_locked_hint(publish, &mut tracker, locked) {
            return Ok(());
        }
    }

    Ok(())
}

fn bind_lock_target(
    bus: &mut impl SessionBusClient,
    explicit_session_id: Option<&str>,
    current_uid: u32,
) -> Result<(LogindSession, String, bool), LogindSessionError> {
    let session = resolve_current_graphical_session(bus, explicit_session_id, current_uid)?;
    let owner = resolve_logind_owner(bus)?;
    add_locked_hint_signal_match(bus, &session, &owner)?;
    let locked = locked_hint(bus, &session)?;
    Ok((session, owner, locked))
}

fn publish_locked_hint<F>(publish: &mut F, tracker: &mut LockedHintTracker, locked: bool) -> bool
where
    F: FnMut(SessionObservation) -> bool,
{
    let Some(event) = tracker.observe(locked) else {
        return true;
    };

    publish(SessionObservation::Event {
        event,
        source: EventSource::LinuxLogind,
        observed_at: Instant::now(),
    })
}

fn current_effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    unsafe { libc::geteuid() }
}

pub fn logind_signal_match() -> BusSignalMatch<'static> {
    BusSignalMatch {
        sender: None,
        path: Some(LOGIND_MANAGER_PATH),
        interface: Some(LOGIND_MANAGER_INTERFACE),
        member: Some("PrepareForSleep"),
    }
}

pub fn add_logind_signal_match(bus: &mut impl SessionBusClient) -> Result<(), SessionBusError> {
    bus.add_signal_match(logind_signal_match())
}

pub fn map_prepare_for_sleep_signal(signal: &BusSignal) -> Option<RuntimeEvent> {
    if signal.path != LOGIND_MANAGER_PATH
        || signal.interface != LOGIND_MANAGER_INTERFACE
        || signal.member != "PrepareForSleep"
    {
        return None;
    }

    match signal.body.as_slice() {
        [BusValue::Bool(preparing)] => {
            Some(RuntimeEvent::from_logind_prepare_for_sleep(*preparing))
        }
        _ => None,
    }
}

pub fn acquire_sleep_delay_inhibitor(
    bus: &mut impl SessionBusClient,
) -> Result<OwnedFd, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "Inhibit",
        )
        .with_body(vec![
            BusValue::String("sleep".to_string()),
            BusValue::String(LOGIND_INHIBIT_WHO.to_string()),
            BusValue::String(LOGIND_INHIBIT_WHY.to_string()),
            BusValue::String("delay".to_string()),
        ]),
    )?
    .single_unix_fd()
}

pub fn preparing_for_sleep(bus: &mut impl SessionBusClient) -> Result<bool, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            LOGIND_MANAGER_PATH,
            DBUS_PROPERTIES_INTERFACE,
            "Get",
        )
        .with_body(vec![
            BusValue::String(LOGIND_MANAGER_INTERFACE.to_string()),
            BusValue::String("PreparingForSleep".to_string()),
        ]),
    )?
    .single_bool()
}

pub fn resolve_current_graphical_session(
    bus: &mut impl SessionBusClient,
    explicit_session_id: Option<&str>,
    current_uid: u32,
) -> Result<LogindSession, LogindSessionError> {
    if let Some(id) = explicit_session_id.filter(|id| !id.is_empty()) {
        let path = bus
            .call_method(
                BusMethodCall::new(
                    LOGIND_SERVICE_NAME,
                    LOGIND_MANAGER_PATH,
                    LOGIND_MANAGER_INTERFACE,
                    "GetSession",
                )
                .with_body(vec![BusValue::String(id.to_string())]),
            )?
            .single_object_path()?
            .to_string();
        let session = LogindSession {
            id: id.to_string(),
            path,
        };
        return session_properties(bus, &session)?
            .is_eligible_for_uid(current_uid)
            .then_some(session)
            .ok_or(LogindSessionError::NoCurrentGraphicalSession);
    }

    let reply = bus.call_method(BusMethodCall::new(
        LOGIND_SERVICE_NAME,
        LOGIND_MANAGER_PATH,
        LOGIND_MANAGER_INTERFACE,
        "ListSessions",
    ))?;
    let [BusValue::Array(entries)] = reply.body.as_slice() else {
        return Err(LogindSessionError::MalformedReply(
            "one array of session records",
        ));
    };

    let mut matches = Vec::new();
    for entry in entries {
        let BusValue::Struct(fields) = entry else {
            return Err(LogindSessionError::MalformedReply("session record struct"));
        };
        let [BusValue::String(id), BusValue::U32(uid), BusValue::String(_user), BusValue::String(_seat), BusValue::ObjectPath(path)] =
            fields.as_slice()
        else {
            return Err(LogindSessionError::MalformedReply(
                "session record (id, uid, user, seat, object path)",
            ));
        };
        if *uid != current_uid {
            continue;
        }

        let session = LogindSession {
            id: id.clone(),
            path: path.clone(),
        };
        if session_properties(bus, &session)?.is_eligible_for_uid(current_uid) {
            matches.push(session);
        }
    }

    match matches.len() {
        0 => Err(LogindSessionError::NoCurrentGraphicalSession),
        1 => Ok(matches.remove(0)),
        _ => Err(LogindSessionError::AmbiguousGraphicalSessions(
            matches.into_iter().map(|session| session.id).collect(),
        )),
    }
}

pub fn resolve_logind_owner(bus: &mut impl SessionBusClient) -> Result<String, SessionBusError> {
    get_name_owner(bus, LOGIND_SERVICE_NAME)
}

pub fn logind_owner_signal_match() -> BusSignalMatch<'static> {
    BusSignalMatch {
        sender: Some(DBUS_SERVICE_NAME),
        path: Some(DBUS_OBJECT_PATH),
        interface: Some(DBUS_INTERFACE),
        member: Some("NameOwnerChanged"),
    }
}

pub fn add_logind_owner_signal_match(
    bus: &mut impl SessionBusClient,
) -> Result<(), SessionBusError> {
    bus.add_signal_match(logind_owner_signal_match())
}

pub fn logind_owner_changed(signal: &BusSignal) -> Option<Option<String>> {
    if signal.sender.as_deref() != Some(DBUS_SERVICE_NAME) {
        return None;
    }

    let owner_change = parse_name_owner_changed_signal(signal)?;
    if owner_change.name != LOGIND_SERVICE_NAME {
        return None;
    }

    Some(owner_change.new_owner)
}

pub fn locked_hint_signal_match<'a>(
    session_path: &'a str,
    logind_owner: &'a str,
) -> BusSignalMatch<'a> {
    BusSignalMatch {
        sender: Some(logind_owner),
        path: Some(session_path),
        interface: Some(DBUS_PROPERTIES_INTERFACE),
        member: Some("PropertiesChanged"),
    }
}

pub fn add_locked_hint_signal_match(
    bus: &mut impl SessionBusClient,
    session: &LogindSession,
    logind_owner: &str,
) -> Result<(), SessionBusError> {
    bus.add_signal_match(locked_hint_signal_match(&session.path, logind_owner))
}

pub fn locked_hint(
    bus: &mut impl SessionBusClient,
    session: &LogindSession,
) -> Result<bool, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            &session.path,
            DBUS_PROPERTIES_INTERFACE,
            "Get",
        )
        .with_body(vec![
            BusValue::String(LOGIND_SESSION_INTERFACE.to_string()),
            BusValue::String("LockedHint".to_string()),
        ]),
    )?
    .single_bool()
}

pub fn map_locked_hint_change(
    signal: &BusSignal,
    session: &LogindSession,
    logind_owner: &str,
) -> Option<LockedHintChange> {
    if signal.sender.as_deref() != Some(logind_owner)
        || signal.path != session.path
        || signal.interface != DBUS_PROPERTIES_INTERFACE
        || signal.member != "PropertiesChanged"
    {
        return None;
    }

    let [BusValue::String(interface), BusValue::Dict(changed), BusValue::Array(invalidated)] =
        signal.body.as_slice()
    else {
        return None;
    };
    if interface != LOGIND_SESSION_INTERFACE {
        return None;
    }

    for (key, value) in changed {
        if matches!(key, BusValue::String(name) if name == "LockedHint") {
            return match value {
                BusValue::Variant(value) => match value.as_ref() {
                    BusValue::Bool(locked) => Some(LockedHintChange::Changed(*locked)),
                    _ => None,
                },
                BusValue::Bool(locked) => Some(LockedHintChange::Changed(*locked)),
                _ => None,
            };
        }
    }

    invalidated
        .iter()
        .any(|value| matches!(value, BusValue::String(name) if name == "LockedHint"))
        .then_some(LockedHintChange::Invalidated)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogindSessionProperties {
    uid: u32,
    remote: bool,
    session_type: String,
    class: String,
    active: bool,
}

impl LogindSessionProperties {
    fn is_eligible_for_uid(&self, uid: u32) -> bool {
        self.uid == uid
            && !self.remote
            && self.active
            && GRAPHICAL_SESSION_TYPES.contains(&self.session_type.as_str())
            && USER_SESSION_CLASSES.contains(&self.class.as_str())
    }
}

fn session_properties(
    bus: &mut impl SessionBusClient,
    session: &LogindSession,
) -> Result<LogindSessionProperties, LogindSessionError> {
    let reply = bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            &session.path,
            DBUS_PROPERTIES_INTERFACE,
            "GetAll",
        )
        .with_body(vec![BusValue::String(LOGIND_SESSION_INTERFACE.to_string())]),
    )?;
    let [BusValue::Dict(properties)] = reply.body.as_slice() else {
        return Err(LogindSessionError::MalformedReply(
            "one session property dictionary",
        ));
    };

    let user = property(properties, "User")?;
    let BusValue::Struct(user) = unwrap_variant(user) else {
        return Err(LogindSessionError::MalformedReply("User struct"));
    };
    let [BusValue::U32(uid), BusValue::ObjectPath(_user_path)] = user.as_slice() else {
        return Err(LogindSessionError::MalformedReply(
            "User (uid, object path)",
        ));
    };

    Ok(LogindSessionProperties {
        uid: *uid,
        remote: property_bool(properties, "Remote")?,
        session_type: property_string(properties, "Type")?.to_string(),
        class: property_string(properties, "Class")?.to_string(),
        active: property_bool(properties, "Active")?,
    })
}

fn property<'a>(
    properties: &'a [(BusValue, BusValue)],
    name: &'static str,
) -> Result<&'a BusValue, LogindSessionError> {
    properties
        .iter()
        .find_map(|(key, value)| match key {
            BusValue::String(key) if key == name => Some(value),
            _ => None,
        })
        .ok_or(LogindSessionError::MalformedReply(name))
}

fn unwrap_variant(value: &BusValue) -> &BusValue {
    match value {
        BusValue::Variant(value) => value,
        value => value,
    }
}

fn property_bool(
    properties: &[(BusValue, BusValue)],
    name: &'static str,
) -> Result<bool, LogindSessionError> {
    match unwrap_variant(property(properties, name)?) {
        BusValue::Bool(value) => Ok(*value),
        _ => Err(LogindSessionError::MalformedReply(name)),
    }
}

fn property_string<'a>(
    properties: &'a [(BusValue, BusValue)],
    name: &'static str,
) -> Result<&'a str, LogindSessionError> {
    match unwrap_variant(property(properties, name)?) {
        BusValue::String(value) => Ok(value),
        _ => Err(LogindSessionError::MalformedReply(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_sleep_delay_inhibitor, add_locked_hint_signal_match, add_logind_owner_signal_match,
        add_logind_signal_match, locked_hint, locked_hint_signal_match, logind_owner_changed,
        logind_owner_signal_match, logind_signal_match, map_locked_hint_change,
        map_prepare_for_sleep_signal, resolve_current_graphical_session,
        run_lock_observer_for_session, LockedHintChange, LockedHintTracker, LogindSession,
        LogindSessionError, LOGIND_INHIBIT_WHO, LOGIND_INHIBIT_WHY, LOGIND_MANAGER_INTERFACE,
        LOGIND_MANAGER_PATH, LOGIND_SERVICE_NAME, LOGIND_SESSION_INTERFACE,
    };
    use super::{preparing_for_sleep, DBUS_PROPERTIES_INTERFACE};
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::session::{SessionEvent, SessionObservation};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use std::collections::VecDeque;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    const LOGIND_OWNER: &str = ":1.42";

    #[derive(Debug, Default)]
    struct FakeBus {
        calls: Vec<(String, String, String, String, Vec<BusValue>)>,
        matches: Vec<OwnedBusSignalMatch>,
        replies: VecDeque<BusReply>,
        process_results: VecDeque<Result<Option<BusSignal>, SessionBusError>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OwnedBusSignalMatch {
        sender: Option<String>,
        path: Option<String>,
        interface: Option<String>,
        member: Option<String>,
    }

    impl<'a> From<BusSignalMatch<'a>> for OwnedBusSignalMatch {
        fn from(value: BusSignalMatch<'a>) -> Self {
            Self {
                sender: value.sender.map(ToOwned::to_owned),
                path: value.path.map(ToOwned::to_owned),
                interface: value.interface.map(ToOwned::to_owned),
                member: value.member.map(ToOwned::to_owned),
            }
        }
    }

    impl SessionBusClient for FakeBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            unreachable!("name probing is not used by logind tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
                call.body,
            ));
            self.replies
                .pop_front()
                .ok_or_else(|| SessionBusError::Transport("missing fake reply".to_string()))
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.matches.push(OwnedBusSignalMatch::from(rule));
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            self.process_results
                .pop_front()
                .expect("test should provide every expected bus process result")
        }
    }

    fn prepare_for_sleep_signal(value: bool) -> BusSignal {
        BusSignal::new(
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "PrepareForSleep",
        )
        .with_sender(LOGIND_SERVICE_NAME)
        .with_body(vec![BusValue::Bool(value)])
    }

    fn variant(value: BusValue) -> BusValue {
        BusValue::Variant(Box::new(value))
    }

    fn session_properties_reply(
        uid: u32,
        remote: bool,
        session_type: &str,
        class: &str,
        active: bool,
    ) -> BusReply {
        BusReply::new(vec![BusValue::Dict(vec![
            (
                BusValue::String("User".to_string()),
                variant(BusValue::Struct(vec![
                    BusValue::U32(uid),
                    BusValue::ObjectPath(format!("/org/freedesktop/login1/user/_{uid}")),
                ])),
            ),
            (
                BusValue::String("Remote".to_string()),
                variant(BusValue::Bool(remote)),
            ),
            (
                BusValue::String("Type".to_string()),
                variant(BusValue::String(session_type.to_string())),
            ),
            (
                BusValue::String("Class".to_string()),
                variant(BusValue::String(class.to_string())),
            ),
            (
                BusValue::String("Active".to_string()),
                variant(BusValue::Bool(active)),
            ),
        ])])
    }

    fn session_record(id: &str, uid: u32, path: &str) -> BusValue {
        BusValue::Struct(vec![
            BusValue::String(id.to_string()),
            BusValue::U32(uid),
            BusValue::String("user".to_string()),
            BusValue::String("seat0".to_string()),
            BusValue::ObjectPath(path.to_string()),
        ])
    }

    fn session(id: &str) -> LogindSession {
        LogindSession {
            id: id.to_string(),
            path: format!("/org/freedesktop/login1/session/_{id}"),
        }
    }

    fn properties_changed(
        session: &LogindSession,
        changed: Vec<(BusValue, BusValue)>,
        invalidated: Vec<BusValue>,
    ) -> BusSignal {
        BusSignal::new(
            &session.path,
            DBUS_PROPERTIES_INTERFACE,
            "PropertiesChanged",
        )
        .with_sender(LOGIND_OWNER)
        .with_body(vec![
            BusValue::String(LOGIND_SESSION_INTERFACE.to_string()),
            BusValue::Dict(changed),
            BusValue::Array(invalidated),
        ])
    }

    fn owner_changed(name: &str, old_owner: &str, new_owner: &str) -> BusSignal {
        BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(name.to_string()),
                BusValue::String(old_owner.to_string()),
                BusValue::String(new_owner.to_string()),
            ])
    }

    #[test]
    fn prepare_for_sleep_true_maps_to_machine_preparing_event() {
        assert_eq!(
            map_prepare_for_sleep_signal(&prepare_for_sleep_signal(true)),
            Some(RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::MachinePreparingForSleep
            ))
        );
    }

    #[test]
    fn prepare_for_sleep_false_maps_to_machine_resumed_event() {
        assert_eq!(
            map_prepare_for_sleep_signal(&prepare_for_sleep_signal(false)),
            Some(RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::MachineResumed
            ))
        );
    }

    #[test]
    fn unrelated_or_malformed_logind_signals_are_ignored() {
        let wrong_member = BusSignal::new(LOGIND_MANAGER_PATH, LOGIND_MANAGER_INTERFACE, "Lock");
        let malformed = BusSignal::new(
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "PrepareForSleep",
        )
        .with_body(vec![BusValue::String("true".to_string())]);

        assert_eq!(map_prepare_for_sleep_signal(&wrong_member), None);
        assert_eq!(map_prepare_for_sleep_signal(&malformed), None);
    }

    #[test]
    fn logind_signal_match_targets_prepare_for_sleep_without_sender_owner() {
        assert_eq!(
            logind_signal_match(),
            BusSignalMatch {
                sender: None,
                path: Some(LOGIND_MANAGER_PATH),
                interface: Some(LOGIND_MANAGER_INTERFACE),
                member: Some("PrepareForSleep"),
            }
        );
    }

    #[test]
    fn add_logind_signal_match_registers_prepare_for_sleep_match() {
        let mut bus = FakeBus::default();

        add_logind_signal_match(&mut bus).expect("add logind signal match");

        assert_eq!(
            bus.matches,
            vec![OwnedBusSignalMatch::from(logind_signal_match())]
        );
    }

    #[test]
    fn acquire_sleep_delay_inhibitor_calls_logind_inhibit() {
        let mut pipe_fds = [0; 2];
        let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(pipe_result, 0, "test pipe should be created");

        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::UnixFd(pipe_fds[0])]));

        let inhibitor =
            acquire_sleep_delay_inhibitor(&mut bus).expect("acquire sleep delay inhibitor");

        assert_eq!(inhibitor.as_raw_fd(), pipe_fds[0]);
        assert_eq!(
            bus.calls,
            vec![(
                LOGIND_SERVICE_NAME.to_string(),
                LOGIND_MANAGER_PATH.to_string(),
                LOGIND_MANAGER_INTERFACE.to_string(),
                "Inhibit".to_string(),
                vec![
                    BusValue::String("sleep".to_string()),
                    BusValue::String(LOGIND_INHIBIT_WHO.to_string()),
                    BusValue::String(LOGIND_INHIBIT_WHY.to_string()),
                    BusValue::String("delay".to_string()),
                ],
            )]
        );

        drop(inhibitor);
        unsafe {
            libc::close(pipe_fds[1]);
        }
    }

    #[test]
    fn preparing_for_sleep_reads_logind_property() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Variant(Box::new(
                BusValue::Bool(true),
            ))]));

        let preparing = preparing_for_sleep(&mut bus).expect("read PreparingForSleep");

        assert!(preparing);
        assert_eq!(
            bus.calls,
            vec![(
                LOGIND_SERVICE_NAME.to_string(),
                LOGIND_MANAGER_PATH.to_string(),
                DBUS_PROPERTIES_INTERFACE.to_string(),
                "Get".to_string(),
                vec![
                    BusValue::String(LOGIND_MANAGER_INTERFACE.to_string()),
                    BusValue::String("PreparingForSleep".to_string()),
                ],
            )]
        );
    }

    #[test]
    fn preparing_for_sleep_accepts_unwrapped_bool_for_tests() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Bool(false)]));

        assert!(!preparing_for_sleep(&mut bus).expect("read PreparingForSleep"));
    }

    #[test]
    fn preparing_for_sleep_rejects_unexpected_property_shape() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Variant(Box::new(
                BusValue::String("false".to_string()),
            ))]));

        let err = preparing_for_sleep(&mut bus).expect_err("malformed property should fail");

        assert_eq!(
            err,
            SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "string",
            }
        );
    }

    #[test]
    fn explicit_session_is_resolved_only_when_it_is_the_users_active_graphical_session() {
        let expected = session("34");
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::ObjectPath(
                expected.path.clone(),
            )]));
        bus.replies.push_back(session_properties_reply(
            1000, false, "wayland", "user", true,
        ));

        assert_eq!(
            resolve_current_graphical_session(&mut bus, Some("34"), 1000),
            Ok(expected)
        );
        assert_eq!(bus.calls[0].3, "GetSession");
        assert_eq!(bus.calls[1].3, "GetAll");
    }

    #[test]
    fn explicit_remote_or_other_user_session_is_rejected() {
        for properties in [
            session_properties_reply(1000, true, "wayland", "user", true),
            session_properties_reply(1001, false, "wayland", "user", true),
        ] {
            let expected = session("34");
            let mut bus = FakeBus::default();
            bus.replies
                .push_back(BusReply::new(vec![BusValue::ObjectPath(
                    expected.path.clone(),
                )]));
            bus.replies.push_back(properties);

            assert_eq!(
                resolve_current_graphical_session(&mut bus, Some("34"), 1000),
                Err(LogindSessionError::NoCurrentGraphicalSession)
            );
        }
    }

    #[test]
    fn discovery_ignores_non_graphical_non_user_and_inactive_sessions() {
        let selected = session("34");
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Array(vec![
                session_record("other-user", 1001, "/other-user"),
                session_record("manager", 1000, "/manager"),
                session_record("remote", 1000, "/remote"),
                session_record("greeter", 1000, "/greeter"),
                session_record("inactive", 1000, "/inactive"),
                session_record("34", 1000, &selected.path),
            ])]));
        bus.replies.push_back(session_properties_reply(
            1000,
            false,
            "unspecified",
            "manager",
            true,
        ));
        bus.replies
            .push_back(session_properties_reply(1000, true, "tty", "user", true));
        bus.replies.push_back(session_properties_reply(
            1000, false, "wayland", "greeter", true,
        ));
        bus.replies.push_back(session_properties_reply(
            1000, false, "wayland", "user", false,
        ));
        bus.replies.push_back(session_properties_reply(
            1000, false, "wayland", "user", true,
        ));

        assert_eq!(
            resolve_current_graphical_session(&mut bus, None, 1000),
            Ok(selected)
        );
        assert_eq!(
            bus.calls.iter().filter(|call| call.3 == "GetAll").count(),
            5,
            "other-user sessions must be discarded before reading their properties"
        );
    }

    #[test]
    fn discovery_refuses_to_guess_between_multiple_graphical_sessions() {
        let first = session("8");
        let second = session("34");
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Array(vec![
                session_record("8", 1000, &first.path),
                session_record("34", 1000, &second.path),
            ])]));
        bus.replies
            .push_back(session_properties_reply(1000, false, "x11", "user", true));
        bus.replies.push_back(session_properties_reply(
            1000, false, "wayland", "user", true,
        ));

        assert_eq!(
            resolve_current_graphical_session(&mut bus, None, 1000),
            Err(LogindSessionError::AmbiguousGraphicalSessions(vec![
                "8".to_string(),
                "34".to_string(),
            ]))
        );
    }

    #[test]
    fn locked_hint_reads_the_target_session_property() {
        let target = session("34");
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![variant(BusValue::Bool(true))]));

        assert!(locked_hint(&mut bus, &target).expect("read LockedHint"));
        assert_eq!(bus.calls.len(), 1);
        assert_eq!(bus.calls[0].1, target.path);
        assert_eq!(bus.calls[0].3, "Get");
        assert_eq!(
            bus.calls[0].4,
            vec![
                BusValue::String(LOGIND_SESSION_INTERFACE.to_string()),
                BusValue::String("LockedHint".to_string()),
            ]
        );
    }

    #[test]
    fn locked_hint_signal_match_targets_only_the_resolved_session() {
        let target = session("34");
        let mut bus = FakeBus::default();

        add_locked_hint_signal_match(&mut bus, &target, LOGIND_OWNER)
            .expect("add LockedHint match");

        assert_eq!(
            bus.matches,
            vec![OwnedBusSignalMatch::from(locked_hint_signal_match(
                &target.path,
                LOGIND_OWNER,
            ))]
        );
    }

    #[test]
    fn logind_owner_changes_are_subscribed_and_mapped() {
        let mut bus = FakeBus::default();

        add_logind_owner_signal_match(&mut bus).expect("add logind owner match");

        assert_eq!(
            bus.matches,
            vec![OwnedBusSignalMatch::from(logind_owner_signal_match())]
        );
        assert_eq!(
            logind_owner_changed(&owner_changed(LOGIND_SERVICE_NAME, ":1.42", ":1.99")),
            Some(Some(":1.99".to_string()))
        );
        assert_eq!(
            logind_owner_changed(&owner_changed(LOGIND_SERVICE_NAME, ":1.42", "")),
            Some(None)
        );
        assert_eq!(
            logind_owner_changed(&owner_changed("org.example.Other", ":1.42", ":1.99")),
            None
        );
        assert_eq!(
            logind_owner_changed(
                &owner_changed(LOGIND_SERVICE_NAME, ":1.42", ":1.99").with_sender(":1.1")
            ),
            None
        );
    }

    #[test]
    fn locked_hint_changes_and_invalidation_are_mapped() {
        let target = session("34");
        let changed = properties_changed(
            &target,
            vec![(
                BusValue::String("LockedHint".to_string()),
                variant(BusValue::Bool(true)),
            )],
            Vec::new(),
        );
        let invalidated = properties_changed(
            &target,
            Vec::new(),
            vec![BusValue::String("LockedHint".to_string())],
        );

        assert_eq!(
            map_locked_hint_change(&changed, &target, LOGIND_OWNER),
            Some(LockedHintChange::Changed(true))
        );
        assert_eq!(
            map_locked_hint_change(&invalidated, &target, LOGIND_OWNER),
            Some(LockedHintChange::Invalidated)
        );
        assert_eq!(
            map_locked_hint_change(
                &properties_changed(&session("35"), Vec::new(), Vec::new()),
                &target,
                LOGIND_OWNER,
            ),
            None
        );
        assert_eq!(
            map_locked_hint_change(&changed.clone().with_sender(":1.99"), &target, LOGIND_OWNER),
            None
        );
    }

    #[test]
    fn lock_observer_reconciles_initial_locked_hint() {
        let target = session("34");
        let mut bus = FakeBus::default();
        bus.replies.extend([
            BusReply::new(vec![BusValue::ObjectPath(target.path.clone())]),
            session_properties_reply(1000, false, "wayland", "user", true),
            BusReply::new(vec![BusValue::String(LOGIND_OWNER.to_string())]),
            BusReply::new(vec![variant(BusValue::Bool(true))]),
        ]);
        let stop = AtomicBool::new(true);
        let mut observations = Vec::new();

        run_lock_observer_for_session(
            &mut bus,
            &mut |observation| {
                observations.push(observation);
                true
            },
            &stop,
            Some("34"),
            1000,
        )
        .expect("initial LockedHint should be reconciled before the signal loop");

        assert!(matches!(
            observations.as_slice(),
            [SessionObservation::Event {
                event: SessionEvent::Lock,
                source: EventSource::LinuxLogind,
                ..
            }]
        ));
        assert_eq!(bus.matches.len(), 2);
        assert_eq!(
            bus.calls
                .iter()
                .map(|call| call.3.as_str())
                .collect::<Vec<_>>(),
            vec!["GetSession", "GetAll", "GetNameOwner", "Get"]
        );
    }

    #[test]
    fn lock_observer_rebinds_after_logind_restarts() {
        let target = session("34");
        let bind_replies = |owner: &str| {
            [
                BusReply::new(vec![BusValue::ObjectPath(target.path.clone())]),
                session_properties_reply(1000, false, "wayland", "user", true),
                BusReply::new(vec![BusValue::String(owner.to_string())]),
                BusReply::new(vec![variant(BusValue::Bool(false))]),
            ]
        };
        let mut bus = FakeBus::default();
        bus.replies.extend(bind_replies(LOGIND_OWNER));
        bus.replies.extend(bind_replies(":1.99"));
        bus.process_results.extend([
            Ok(Some(owner_changed(LOGIND_SERVICE_NAME, LOGIND_OWNER, ""))),
            Ok(Some(owner_changed(LOGIND_SERVICE_NAME, "", ":1.99"))),
            Ok(Some(
                properties_changed(
                    &target,
                    vec![(
                        BusValue::String("LockedHint".to_string()),
                        variant(BusValue::Bool(true)),
                    )],
                    Vec::new(),
                )
                .with_sender(":1.99"),
            )),
            Err(SessionBusError::Transport("stop test loop".to_string())),
        ]);
        let stop = AtomicBool::new(false);
        let mut observations = Vec::new();

        assert_eq!(
            run_lock_observer_for_session(
                &mut bus,
                &mut |observation| {
                    observations.push(observation);
                    true
                },
                &stop,
                Some("34"),
                1000,
            ),
            Err(LogindSessionError::Bus(SessionBusError::Transport(
                "stop test loop".to_string()
            )))
        );

        assert!(matches!(
            observations.as_slice(),
            [SessionObservation::Event {
                event: SessionEvent::Lock,
                source: EventSource::LinuxLogind,
                ..
            }]
        ));
        assert_eq!(bus.matches.len(), 3);
        assert_eq!(
            bus.calls
                .iter()
                .map(|call| call.3.as_str())
                .collect::<Vec<_>>(),
            vec![
                "GetSession",
                "GetAll",
                "GetNameOwner",
                "Get",
                "GetSession",
                "GetAll",
                "GetNameOwner",
                "Get",
            ]
        );
    }

    #[test]
    fn locked_hint_tracker_reconciles_initial_lock_and_deduplicates_transitions() {
        let mut tracker = LockedHintTracker::default();

        assert_eq!(
            tracker.observe(true),
            Some(crate::session::SessionEvent::Lock)
        );
        assert_eq!(tracker.observe(true), None);
        assert_eq!(
            tracker.observe(false),
            Some(crate::session::SessionEvent::Unlock)
        );
        assert_eq!(tracker.observe(false), None);
        assert_eq!(
            tracker.observe(true),
            Some(crate::session::SessionEvent::Lock)
        );
    }

    #[test]
    fn initial_unlocked_hint_does_not_emit_an_unlock_event() {
        let mut tracker = LockedHintTracker::default();

        assert_eq!(tracker.observe(false), None);
    }
}
