use dbus::arg::messageitem::MessageItem as DbusMessageItem;
use dbus::blocking::{BlockingSender as DbusBlockingSender, Connection as DbusConnection};
use dbus::message::{MatchRule as DbusMatchRule, MessageType as DbusMessageType};
use dbus::Message as DbusMessage;
use std::fmt;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

const SESSION_BUS_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DBUS_METHOD_CALL_TIMEOUT: Duration = Duration::from_secs(1);
pub const DBUS_SERVICE_NAME: &str = "org.freedesktop.DBus";
pub const DBUS_OBJECT_PATH: &str = "/org/freedesktop/DBus";
pub const DBUS_INTERFACE: &str = "org.freedesktop.DBus";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBusError {
    Transport(String),
    Timeout {
        name: String,
        timeout: Duration,
    },
    UnexpectedReplyShape {
        expected: &'static str,
        actual: &'static str,
    },
    UnsupportedMessageBody {
        context: &'static str,
        kind: &'static str,
    },
}

impl fmt::Display for SessionBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "{message}"),
            Self::Timeout { name, timeout } => {
                write!(
                    f,
                    "timed out waiting for bus name `{name}` after {timeout:?}"
                )
            }
            Self::UnexpectedReplyShape { expected, actual } => {
                write!(
                    f,
                    "unexpected bus reply shape: expected {expected}, got {actual}"
                )
            }
            Self::UnsupportedMessageBody { context, kind } => {
                write!(f, "unsupported D-Bus {context}: {kind}")
            }
        }
    }
}

impl std::error::Error for SessionBusError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusValue {
    Bool(bool),
    UnixFd(RawFd),
    U32(u32),
    U64(u64),
    String(String),
    ObjectPath(String),
    Array(Vec<BusValue>),
    Struct(Vec<BusValue>),
    Dict(Vec<(BusValue, BusValue)>),
    Variant(Box<BusValue>),
}

impl BusValue {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::UnixFd(_) => "fd",
            Self::U32(_) => "u32",
            Self::U64(_) => "u64",
            Self::String(_) => "string",
            Self::ObjectPath(_) => "object path",
            Self::Array(_) => "array",
            Self::Struct(_) => "struct",
            Self::Dict(_) => "dict",
            Self::Variant(_) => "variant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusReply {
    pub body: Vec<BusValue>,
}

impl BusReply {
    pub fn new(body: Vec<BusValue>) -> Self {
        Self { body }
    }

    pub fn single_bool(&self) -> Result<bool, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::Bool(value)] => Ok(*value),
            [BusValue::Variant(value)] => match value.as_ref() {
                BusValue::Bool(value) => Ok(*value),
                value => Err(SessionBusError::UnexpectedReplyShape {
                    expected: "single bool",
                    actual: value.kind(),
                }),
            },
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_u64(&self) -> Result<u64, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::U64(value)] => Ok(*value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u64",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u64",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_u32(&self) -> Result<u32, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::U32(value)] => Ok(*value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u32",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u32",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_unix_fd(self) -> Result<OwnedFd, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::UnixFd(fd)] => {
                let fd = *fd;
                // SAFETY: D-Bus transferred ownership of this descriptor into the
                // reply. `BusValue` does not close raw descriptors on drop, so
                // this creates the single owned handle responsible for closing it.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            }
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single fd",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single fd",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_string(&self) -> Result<&str, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::String(value)] => Ok(value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single string",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single string",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_object_path(&self) -> Result<&str, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::ObjectPath(value)] => Ok(value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single object path",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single object path",
                actual: "multiple values",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusMethodCall<'a> {
    pub destination: &'a str,
    pub path: &'a str,
    pub interface: &'a str,
    pub member: &'a str,
    pub body: Vec<BusValue>,
}

impl<'a> BusMethodCall<'a> {
    pub fn new(destination: &'a str, path: &'a str, interface: &'a str, member: &'a str) -> Self {
        Self {
            destination,
            path,
            interface,
            member,
            body: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: Vec<BusValue>) -> Self {
        self.body = body;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusSignalMatch<'a> {
    pub sender: Option<&'a str>,
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
}

impl<'a> BusSignalMatch<'a> {
    pub fn matches(&self, signal: &BusSignal) -> bool {
        if let Some(sender) = self.sender {
            if signal.sender.as_deref() != Some(sender) {
                return false;
            }
        }

        if let Some(path) = self.path {
            if signal.path != path {
                return false;
            }
        }

        if let Some(interface) = self.interface {
            if signal.interface != interface {
                return false;
            }
        }

        if let Some(member) = self.member {
            if signal.member != member {
                return false;
            }
        }

        true
    }
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

impl OwnedBusSignalMatch {
    fn as_match_rule(&self) -> DbusMatchRule<'static> {
        let mut rule = DbusMatchRule::new().with_type(DbusMessageType::Signal);
        if let Some(sender) = &self.sender {
            rule = rule.with_sender(sender.clone());
        }
        if let Some(path) = &self.path {
            rule = rule.with_path(path.clone());
        }
        if let Some(interface) = &self.interface {
            rule = rule.with_interface(interface.clone());
        }
        if let Some(member) = &self.member {
            rule = rule.with_member(member.clone());
        }
        rule
    }

    fn matches(&self, signal: &BusSignal) -> bool {
        if let Some(sender) = &self.sender {
            if signal.sender.as_ref() != Some(sender) {
                return false;
            }
        }

        if let Some(path) = &self.path {
            if &signal.path != path {
                return false;
            }
        }

        if let Some(interface) = &self.interface {
            if &signal.interface != interface {
                return false;
            }
        }

        if let Some(member) = &self.member {
            if &signal.member != member {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusSignal {
    pub sender: Option<String>,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub body: Vec<BusValue>,
}

impl BusSignal {
    pub fn new(
        path: impl Into<String>,
        interface: impl Into<String>,
        member: impl Into<String>,
    ) -> Self {
        Self {
            sender: None,
            path: path.into(),
            interface: interface.into(),
            member: member.into(),
            body: Vec::new(),
        }
    }

    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = Some(sender.into());
        self
    }

    pub fn with_body(mut self, body: Vec<BusValue>) -> Self {
        self.body = body;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOwnerChanged {
    pub name: String,
    pub old_owner: Option<String>,
    pub new_owner: Option<String>,
}

pub fn get_name_owner(
    bus: &mut impl SessionBusClient,
    name: &str,
) -> Result<String, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            DBUS_SERVICE_NAME,
            DBUS_OBJECT_PATH,
            DBUS_INTERFACE,
            "GetNameOwner",
        )
        .with_body(vec![BusValue::String(name.to_string())]),
    )?
    .single_string()
    .map(str::to_owned)
}

pub fn parse_name_owner_changed_signal(signal: &BusSignal) -> Option<NameOwnerChanged> {
    if signal.path != DBUS_OBJECT_PATH
        || signal.interface != DBUS_INTERFACE
        || signal.member != "NameOwnerChanged"
    {
        return None;
    }

    let [BusValue::String(name), BusValue::String(old_owner), BusValue::String(new_owner)] =
        signal.body.as_slice()
    else {
        return None;
    };

    Some(NameOwnerChanged {
        name: name.clone(),
        old_owner: normalize_dbus_owner(old_owner),
        new_owner: normalize_dbus_owner(new_owner),
    })
}

fn normalize_dbus_owner(owner: &str) -> Option<String> {
    if owner.is_empty() {
        None
    } else {
        Some(owner.to_string())
    }
}

pub trait SessionBusClient {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError>;
    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError>;
    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError>;
    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError>;

    fn wait_for_name(&mut self, name: &str, timeout: Duration) -> Result<(), SessionBusError> {
        let started = Instant::now();
        loop {
            if self.name_has_owner(name)? {
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(SessionBusError::Timeout {
                    name: name.to_string(),
                    timeout,
                });
            }

            let remaining = timeout.saturating_sub(elapsed);
            let poll_timeout = remaining.min(SESSION_BUS_WAIT_POLL_INTERVAL);
            let _ = self.process(poll_timeout)?;
        }
    }
}

impl<T: SessionBusClient + ?Sized> SessionBusClient for Box<T> {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
        (**self).name_has_owner(name)
    }

    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
        (**self).call_method(call)
    }

    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
        (**self).add_signal_match(rule)
    }

    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
        (**self).process(timeout)
    }

    fn wait_for_name(&mut self, name: &str, timeout: Duration) -> Result<(), SessionBusError> {
        (**self).wait_for_name(name, timeout)
    }
}

pub fn new_session_bus_client() -> Result<Box<dyn SessionBusClient + Send>, SessionBusError> {
    Ok(Box::new(DbusSessionBusClient::new_session()?))
}

pub fn new_system_bus_client() -> Result<Box<dyn SessionBusClient + Send>, SessionBusError> {
    Ok(Box::new(DbusSessionBusClient::new_system()?))
}

pub struct DbusSessionBusClient {
    connection: DbusConnection,
    method_call_timeout: Duration,
    signal_rules: Vec<OwnedBusSignalMatch>,
}

impl DbusSessionBusClient {
    pub fn new_session() -> Result<Self, SessionBusError> {
        Ok(Self {
            connection: DbusConnection::new_session()
                .map_err(|err| SessionBusError::Transport(err.to_string()))?,
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        })
    }

    pub fn new_system() -> Result<Self, SessionBusError> {
        Ok(Self {
            connection: DbusConnection::new_system()
                .map_err(|err| SessionBusError::Transport(err.to_string()))?,
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        })
    }
}

impl SessionBusClient for DbusSessionBusClient {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
        self.call_method(
            BusMethodCall::new(
                DBUS_SERVICE_NAME,
                DBUS_OBJECT_PATH,
                DBUS_INTERFACE,
                "NameHasOwner",
            )
            .with_body(vec![BusValue::String(name.to_string())]),
        )?
        .single_bool()
    }

    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
        let mut message =
            DbusMessage::new_method_call(call.destination, call.path, call.interface, call.member)
                .map_err(SessionBusError::Transport)?;
        for value in call.body {
            message = append_dbus_message_value(message, value)?;
        }

        let reply = DbusBlockingSender::send_with_reply_and_block(
            &self.connection,
            message,
            self.method_call_timeout,
        )
        .map_err(|err| SessionBusError::Transport(err.to_string()))?;

        Ok(BusReply::new(
            reply
                .get_items()
                .into_iter()
                .map(bus_value_from_dbus_message_item)
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
        let rule = OwnedBusSignalMatch::from(rule);
        self.connection
            .add_match_no_cb(&rule.as_match_rule().match_str())
            .map_err(|err| SessionBusError::Transport(err.to_string()))?;
        self.signal_rules.push(rule);
        Ok(())
    }

    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }

            let remaining = timeout.saturating_sub(elapsed);
            let Some(message) = self
                .connection
                .channel()
                .blocking_pop_message(remaining)
                .map_err(|err| SessionBusError::Transport(err.to_string()))?
            else {
                return Ok(None);
            };

            if message.msg_type() != DbusMessageType::Signal {
                continue;
            }

            let signal = bus_signal_from_dbus_message(message)?;
            if self.signal_rules.is_empty()
                || self.signal_rules.iter().any(|rule| rule.matches(&signal))
            {
                return Ok(Some(signal));
            }
        }
    }
}

fn append_dbus_message_value(
    message: DbusMessage,
    value: BusValue,
) -> Result<DbusMessage, SessionBusError> {
    match value {
        BusValue::Bool(value) => Ok(message.append1(value)),
        BusValue::UnixFd(value) => {
            // SAFETY: the descriptor is owned by the caller-provided BusValue for
            // this message construction path.
            let fd = unsafe { dbus::arg::OwnedFd::from_raw_fd(value) };
            Ok(message.append1(fd))
        }
        BusValue::U32(value) => Ok(message.append1(value)),
        BusValue::U64(value) => Ok(message.append1(value)),
        BusValue::String(value) => Ok(message.append1(value)),
        unsupported @ (BusValue::ObjectPath(_)
        | BusValue::Array(_)
        | BusValue::Struct(_)
        | BusValue::Dict(_)
        | BusValue::Variant(_)) => Err(SessionBusError::UnsupportedMessageBody {
            context: "method-call body",
            kind: unsupported.kind(),
        }),
    }
}

fn bus_value_from_dbus_message_item(item: DbusMessageItem) -> Result<BusValue, SessionBusError> {
    match item {
        DbusMessageItem::Bool(value) => Ok(BusValue::Bool(value)),
        DbusMessageItem::UnixFd(value) => Ok(BusValue::UnixFd(value.into_raw_fd())),
        DbusMessageItem::UInt32(value) => Ok(BusValue::U32(value)),
        DbusMessageItem::UInt64(value) => Ok(BusValue::U64(value)),
        DbusMessageItem::Str(value) => Ok(BusValue::String(value)),
        DbusMessageItem::ObjectPath(value) => Ok(BusValue::ObjectPath(value.to_string())),
        DbusMessageItem::Array(values) => values
            .into_vec()
            .into_iter()
            .map(bus_value_from_dbus_message_item)
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Array),
        DbusMessageItem::Struct(values) => values
            .into_iter()
            .map(bus_value_from_dbus_message_item)
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Struct),
        DbusMessageItem::Dict(values) => values
            .into_vec()
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    bus_value_from_dbus_message_item(key)?,
                    bus_value_from_dbus_message_item(value)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Dict),
        DbusMessageItem::Variant(value) => {
            bus_value_from_dbus_message_item(*value).map(|value| BusValue::Variant(Box::new(value)))
        }
        other => Err(SessionBusError::UnexpectedReplyShape {
            expected: "bool/u32/u64/string/object-path/array/struct/dict/fd/variant",
            actual: dbus_message_item_kind(&other),
        }),
    }
}

pub(crate) fn bus_signal_from_dbus_message(
    message: DbusMessage,
) -> Result<BusSignal, SessionBusError> {
    let path = message
        .path()
        .ok_or_else(|| SessionBusError::Transport("signal missing object path".to_string()))?
        .to_string();
    let interface = message
        .interface()
        .ok_or_else(|| SessionBusError::Transport("signal missing interface".to_string()))?
        .to_string();
    let member = message
        .member()
        .ok_or_else(|| SessionBusError::Transport("signal missing member".to_string()))?
        .to_string();
    let sender = message.sender().map(|sender| sender.to_string());
    let body = message
        .get_items()
        .into_iter()
        .map(bus_value_from_dbus_message_item)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(BusSignal {
        sender,
        path,
        interface,
        member,
        body,
    })
}

fn dbus_message_item_kind(item: &DbusMessageItem) -> &'static str {
    match item {
        DbusMessageItem::Bool(_) => "bool",
        DbusMessageItem::UInt64(_) => "u64",
        DbusMessageItem::Str(_) => "string",
        DbusMessageItem::Array(_) => "array",
        DbusMessageItem::Struct(_) => "struct",
        DbusMessageItem::Variant(_) => "variant",
        DbusMessageItem::Dict(_) => "dict",
        DbusMessageItem::ObjectPath(_) => "object path",
        DbusMessageItem::Signature(_) => "signature",
        DbusMessageItem::Byte(_) => "byte",
        DbusMessageItem::Int16(_) => "i16",
        DbusMessageItem::Int32(_) => "i32",
        DbusMessageItem::Int64(_) => "i64",
        DbusMessageItem::UInt16(_) => "u16",
        DbusMessageItem::UInt32(_) => "u32",
        DbusMessageItem::Double(_) => "f64",
        DbusMessageItem::UnixFd(_) => "fd",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_dbus_message_value, bus_value_from_dbus_message_item, get_name_owner,
        parse_name_owner_changed_signal, BusMethodCall, BusReply, BusSignal, BusSignalMatch,
        BusValue, NameOwnerChanged, SessionBusClient, SessionBusError, DBUS_INTERFACE,
        DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use dbus::arg::messageitem::{MessageItem, MessageItemArray, MessageItemDict};
    use dbus::strings::{Path as DbusPath, Signature as DbusSignature};
    use dbus::Message as DbusMessage;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct OwnedBusMethodCall {
        destination: String,
        path: String,
        interface: String,
        member: String,
        body: Vec<BusValue>,
    }

    impl<'a> From<BusMethodCall<'a>> for OwnedBusMethodCall {
        fn from(value: BusMethodCall<'a>) -> Self {
            Self {
                destination: value.destination.to_string(),
                path: value.path.to_string(),
                interface: value.interface.to_string(),
                member: value.member.to_string(),
                body: value.body,
            }
        }
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

    impl OwnedBusSignalMatch {
        fn matches(&self, signal: &BusSignal) -> bool {
            if let Some(sender) = &self.sender {
                if signal.sender.as_ref() != Some(sender) {
                    return false;
                }
            }

            if let Some(path) = &self.path {
                if &signal.path != path {
                    return false;
                }
            }

            if let Some(interface) = &self.interface {
                if &signal.interface != interface {
                    return false;
                }
            }

            if let Some(member) = &self.member {
                if &signal.member != member {
                    return false;
                }
            }

            true
        }
    }

    #[derive(Debug, Default)]
    struct FakeSessionBusClient {
        owners: HashSet<String>,
        replies: HashMap<OwnedBusMethodCall, VecDeque<BusReply>>,
        signal_matches: Vec<OwnedBusSignalMatch>,
        queued_signals: VecDeque<BusSignal>,
        owners_to_activate_on_process: VecDeque<String>,
        process_timeouts: Vec<Duration>,
    }

    impl FakeSessionBusClient {
        fn set_name_owner(&mut self, name: &str, present: bool) {
            if present {
                self.owners.insert(name.to_string());
            } else {
                self.owners.remove(name);
            }
        }

        fn queue_name_owner_on_process(&mut self, name: &str) {
            self.owners_to_activate_on_process
                .push_back(name.to_string());
        }

        fn queue_reply(&mut self, call: BusMethodCall<'_>, reply: BusReply) {
            self.replies
                .entry(call.into())
                .or_default()
                .push_back(reply);
        }

        fn queue_signal(&mut self, signal: BusSignal) {
            self.queued_signals.push_back(signal);
        }
    }

    impl SessionBusClient for FakeSessionBusClient {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            Ok(self.owners.contains(name))
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            let key = OwnedBusMethodCall::from(call);
            self.replies
                .get_mut(&key)
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| {
                    SessionBusError::Transport("no queued reply for method call".to_string())
                })
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.signal_matches.push(rule.into());
            Ok(())
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            self.process_timeouts.push(timeout);

            if let Some(name) = self.owners_to_activate_on_process.pop_front() {
                self.owners.insert(name);
            }

            while let Some(signal) = self.queued_signals.pop_front() {
                if self.signal_matches.is_empty()
                    || self.signal_matches.iter().any(|rule| rule.matches(&signal))
                {
                    return Ok(Some(signal));
                }
            }

            Ok(None)
        }
    }

    #[test]
    fn reply_helpers_decode_expected_shapes() {
        let mut pipe_fds = [0; 2];
        let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(pipe_result, 0, "test pipe should be created");

        assert_eq!(
            BusReply::new(vec![BusValue::Bool(true)]).single_bool(),
            Ok(true)
        );
        assert_eq!(
            BusReply::new(vec![BusValue::Variant(Box::new(BusValue::Bool(false)))]).single_bool(),
            Ok(false)
        );
        assert_eq!(BusReply::new(vec![BusValue::U64(42)]).single_u64(), Ok(42));
        assert_eq!(
            BusReply::new(vec![BusValue::String("hello".to_string())]).single_string(),
            Ok("hello")
        );
        let fd = BusReply::new(vec![BusValue::UnixFd(pipe_fds[0])])
            .single_unix_fd()
            .expect("decode fd");
        assert_eq!(fd.as_raw_fd(), pipe_fds[0]);
        assert_eq!(
            BusReply::new(vec![BusValue::U64(7)]).single_bool(),
            Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "u64",
            })
        );

        drop(fd);
        unsafe {
            libc::close(pipe_fds[1]);
        }
    }

    #[test]
    fn outgoing_method_body_rejects_decode_only_variant_values() {
        let message = DbusMessage::new_method_call(
            "org.example.Service",
            "/org/example/Object",
            "org.example.Interface",
            "Method",
        )
        .expect("valid method call");

        assert_eq!(
            append_dbus_message_value(message, BusValue::Variant(Box::new(BusValue::Bool(true))))
                .expect_err("outgoing variants should be rejected"),
            SessionBusError::UnsupportedMessageBody {
                context: "method-call body",
                kind: "variant",
            }
        );
    }

    #[test]
    fn nested_logind_reply_shapes_decode_into_transport_values() {
        let user = MessageItem::Struct(vec![
            MessageItem::UInt32(1000),
            MessageItem::ObjectPath(
                DbusPath::new("/org/freedesktop/login1/user/_1000").expect("object path"),
            ),
        ]);
        let properties = MessageItem::Dict(
            MessageItemDict::new(
                vec![(
                    MessageItem::Str("User".to_string()),
                    MessageItem::Variant(Box::new(user)),
                )],
                DbusSignature::new("s").expect("key signature"),
                DbusSignature::new("v").expect("value signature"),
            )
            .expect("property dictionary"),
        );
        let invalidated = MessageItem::Array(
            MessageItemArray::new(
                vec![MessageItem::Str("LockedHint".to_string())],
                DbusSignature::new("as").expect("array signature"),
            )
            .expect("invalidated property array"),
        );

        assert_eq!(
            bus_value_from_dbus_message_item(properties).expect("decode properties"),
            BusValue::Dict(vec![(
                BusValue::String("User".to_string()),
                BusValue::Variant(Box::new(BusValue::Struct(vec![
                    BusValue::U32(1000),
                    BusValue::ObjectPath("/org/freedesktop/login1/user/_1000".to_string()),
                ]))),
            )])
        );
        assert_eq!(
            bus_value_from_dbus_message_item(invalidated).expect("decode invalidated array"),
            BusValue::Array(vec![BusValue::String("LockedHint".to_string())])
        );
    }

    #[test]
    fn wait_for_name_returns_when_owner_appears() {
        let mut bus = FakeSessionBusClient::default();
        bus.queue_name_owner_on_process("org.example.Service");

        bus.wait_for_name("org.example.Service", Duration::from_millis(200))
            .expect("name should appear before timeout");

        assert_eq!(bus.process_timeouts, vec![Duration::from_millis(50)]);
    }

    #[test]
    fn wait_for_name_times_out_when_owner_never_appears() {
        let mut bus = FakeSessionBusClient::default();

        let err = bus
            .wait_for_name("org.example.Missing", Duration::from_millis(120))
            .expect_err("missing name should time out");

        assert_eq!(
            err,
            SessionBusError::Timeout {
                name: "org.example.Missing".to_string(),
                timeout: Duration::from_millis(120),
            }
        );
        assert!(!bus.process_timeouts.is_empty());
        assert_eq!(bus.process_timeouts[0], Duration::from_millis(50));
        assert!(bus
            .process_timeouts
            .iter()
            .all(|timeout| *timeout <= Duration::from_millis(50)));
    }

    #[test]
    fn method_calls_use_generic_transport_shapes() {
        let mut bus = FakeSessionBusClient::default();
        let call = BusMethodCall::new(
            "org.example.Service",
            "/org/example/Object",
            "org.example.Interface",
            "Ping",
        )
        .with_body(vec![BusValue::String("hello".to_string())]);
        bus.queue_reply(call.clone(), BusReply::new(vec![BusValue::Bool(true)]));

        let reply = bus.call_method(call).expect("queued reply");

        assert_eq!(reply.single_bool(), Ok(true));
    }

    #[test]
    fn get_name_owner_uses_generic_dbus_endpoint() {
        let mut bus = FakeSessionBusClient::default();
        let call = BusMethodCall::new(
            DBUS_SERVICE_NAME,
            DBUS_OBJECT_PATH,
            DBUS_INTERFACE,
            "GetNameOwner",
        )
        .with_body(vec![BusValue::String("org.gnome.ScreenSaver".to_string())]);
        bus.queue_reply(
            call.clone(),
            BusReply::new(vec![BusValue::String(":1.42".to_string())]),
        );

        assert_eq!(
            get_name_owner(&mut bus, "org.gnome.ScreenSaver"),
            Ok(":1.42".to_string())
        );
    }

    #[test]
    fn process_returns_only_signals_matching_registered_rules() {
        let mut bus = FakeSessionBusClient::default();
        bus.add_signal_match(BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: Some("/org/gnome/ScreenSaver"),
            interface: Some("org.gnome.ScreenSaver"),
            member: Some("ActiveChanged"),
        })
        .expect("register match");

        bus.queue_signal(
            BusSignal::new("/org/example/Other", "org.example.Other", "Changed")
                .with_sender("org.example.Other"),
        );
        bus.queue_signal(
            BusSignal::new(
                "/org/gnome/ScreenSaver",
                "org.gnome.ScreenSaver",
                "ActiveChanged",
            )
            .with_sender("org.gnome.ScreenSaver")
            .with_body(vec![BusValue::Bool(true)]),
        );

        let signal = bus
            .process(Duration::from_millis(10))
            .expect("process signal")
            .expect("matching signal");

        assert_eq!(signal.member, "ActiveChanged");
        assert_eq!(signal.body, vec![BusValue::Bool(true)]);
        assert_eq!(bus.process(Duration::from_millis(10)), Ok(None));
    }

    #[test]
    fn bus_signal_match_handles_partial_rules() {
        let signal = BusSignal::new(
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "WakeUpScreen",
        )
        .with_sender("org.gnome.ScreenSaver");

        let broad_match = BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: None,
            interface: Some("org.gnome.ScreenSaver"),
            member: None,
        };
        let narrow_mismatch = BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: None,
            interface: Some("org.gnome.ScreenSaver"),
            member: Some("ActiveChanged"),
        };

        assert!(broad_match.matches(&signal));
        assert!(!narrow_mismatch.matches(&signal));
    }

    #[test]
    fn parse_name_owner_changed_signal_decodes_unique_owner_updates() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.gnome.ScreenSaver".to_string()),
                BusValue::String(":1.10".to_string()),
                BusValue::String(":1.11".to_string()),
            ]);

        assert_eq!(
            parse_name_owner_changed_signal(&signal),
            Some(NameOwnerChanged {
                name: "org.gnome.ScreenSaver".to_string(),
                old_owner: Some(":1.10".to_string()),
                new_owner: Some(":1.11".to_string()),
            })
        );
    }

    #[test]
    fn parse_name_owner_changed_signal_treats_empty_owners_as_missing() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.gnome.ScreenSaver".to_string()),
                BusValue::String(":1.10".to_string()),
                BusValue::String(String::new()),
            ]);

        assert_eq!(
            parse_name_owner_changed_signal(&signal),
            Some(NameOwnerChanged {
                name: "org.gnome.ScreenSaver".to_string(),
                old_owner: Some(":1.10".to_string()),
                new_owner: None,
            })
        );
    }

    #[test]
    fn name_has_owner_uses_generic_transport_without_methods() {
        let mut bus = FakeSessionBusClient::default();
        bus.set_name_owner("org.example.Service", true);

        assert_eq!(bus.name_has_owner("org.example.Service"), Ok(true));
        assert_eq!(bus.name_has_owner("org.example.Missing"), Ok(false));
    }
}
