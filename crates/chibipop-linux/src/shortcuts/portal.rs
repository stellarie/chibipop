//! `org.freedesktop.portal.GlobalShortcuts`: rung 1 of the trigger
//! channel ladder (ARCHITECTURE.md#input-ladders), on its own thread.
//!
//! **Why a thread and synchronous zbus.** The calloop pump stays synchronous
//! (ARCHITECTURE.md#workspace-and-seams), so this module cannot use an async
//! runtime. zbus provides a blocking API, and the capture portal
//! ([`crate::capture::portal::dbus`]) uses the same design. A session lasts
//! until replacement or process exit, because `Activated` and `Deactivated`
//! arrive while the user holds a key. A [`super::SessionHandle`] owns the
//! thread and its bus connection. Replacement drops the old setup signal
//! queue before it closes the connection and joins the thread. Otherwise, a
//! full queue can block the socket reader before it delivers a pending
//! method reply.
//!
//! **The Request race.** [`crate::portal_request`] installs the response
//! match rule before each portal method call, then waits with a deadline.
//! A `BindShortcuts` call can answer at once when it needs no dialog, and a
//! later subscription would lose that answer. The handle can close the
//! connection while a consent dialog is open. The close releases the waiter
//! and lets the session thread end.
//!
//! **Portal interface facts.** Each session calls `BindShortcuts` one time.
//! The portal can keep a user's previous key and can ignore
//! `preferred_trigger`, so `ListShortcuts` is the source of the confirmed
//! state. Version 2 adds `ConfigureShortcuts`. This module calls it only for
//! an explicit reconfiguration, after the new session is bound.
//!
//! **An app id is mandatory.** A non-sandboxed daemon needs a desktop-entry
//! app id, or xdg-desktop-portal refuses `CreateSession`. [`refusal`] keeps
//! the launch advice and the control-socket fallback actionable.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::portal_request::{
    self, handle_token, mangle_sender, Failure, PORTAL_BUS, PORTAL_PATH, RESPONSE_CANCELLED,
    RESPONSE_SUCCESS,
};
use calloop::channel::SyncSender;
use std::sync::mpsc::TrySendError;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

use super::{Binding, Event, SessionId, ShortcutId};

/// The interface that proves this rung's capability on the session bus.
/// The probe checks advertised capability, not compositor identity.
pub const SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `CreateSession`, `ListShortcuts`, and `ConfigureShortcuts` need no user
/// input, so they answer quickly or indicate a problem.
const QUICK: Duration = Duration::from_secs(10);
/// `BindShortcuts` can show a consent dialog. KDE shows a key list for the
/// user, so this budget covers user action. The control socket already
/// serves, and the pump never blocks.
const BIND: Duration = Duration::from_secs(180);
/// Maximum shortcut signals queued while setup runs. Users press keys
/// slowly. The queue only covers the interval between subscription and pump.
const SIGNAL_QUEUE: usize = 64;

/// Return whether `org.freedesktop.portal.GlobalShortcuts` answers on the
/// session bus. A missing bus and a missing portal produce the same result.
pub fn probe() -> bool {
    version().is_some()
}

/// Return the portal interface version. The value is the lower of the
/// frontend and desktop versions.
pub fn version() -> Option<u32> {
    let conn = Connection::session().ok()?;
    let proxy = shortcuts_proxy(&conn).ok()?;
    proxy.get_property::<u32>("version").ok()
}

/// Register the configured ids and pump their signals until the process ends.
///
/// The session sends the bound set, every accepted press and release, every
/// diagnostic, and its own failure as tagged [`Event`] values. A caller keeps
/// the returned handle and stops it before replacing the configuration.
pub fn spawn(
    session: SessionId,
    preferred: Vec<(ShortcutId, String)>,
    reconfigure: bool,
    tx: SyncSender<Event>,
) -> std::io::Result<super::SessionHandle> {
    let cancel = Arc::new(AtomicBool::new(false));
    let connection = Arc::new(Mutex::new(super::ConnectionState::default()));
    let thread_cancel = cancel.clone();
    let thread_connection = connection.clone();
    let thread = thread::Builder::new()
        .name("chibipop-shortcuts".to_string())
        .spawn(move || {
            let conn = match Connection::session() {
                Ok(conn) => conn,
                Err(err) => {
                    let why = Why::from(format!("no session bus: {err}"));
                    let _ = send_event(
                        &tx,
                        Event::Unavailable {
                            session,
                            reason: why.reason,
                            advice: why.advice,
                        },
                        &thread_cancel,
                    );
                    return;
                }
            };
            {
                let Ok(mut slot) = thread_connection.lock() else { return };
                if thread_cancel.load(Ordering::Acquire) {
                    let _ = conn.close();
                    return;
                }
                slot.connection = Some(conn.clone());
            }
            if let Err(why) = run(
                session,
                &preferred,
                reconfigure,
                &tx,
                &thread_cancel,
                &thread_connection,
                conn,
            ) {
                let _ = send_event(
                    &tx,
                    Event::Unavailable {
                        session,
                        reason: why.reason,
                        advice: why.advice,
                    },
                    &thread_cancel,
                );
            }
            if let Ok(mut slot) = thread_connection.lock() {
                drop(slot.setup_signals.take().map(MessageIterator::into_inner));
                slot.connection.take();
            }
        })?;
    Ok(super::SessionHandle::new(session, cancel, connection, thread))
}

/// Why the rung does not serve.
///
/// Two fields serve two readers. A tray row needs one short clause. The log
/// needs the full detail. Separate fields let the app-id case explain a launch
/// method without a paragraph in a menu.
pub struct Why {
    /// Text for a status row.
    pub reason: String,
    /// Action text, when one exists.
    pub advice: Option<String>,
}

impl From<String> for Why {
    fn from(reason: String) -> Why {
        Why { reason, advice: None }
    }
}

/// Send without allowing a full calloop queue to deadlock session retirement.
/// The short retry gives the event loop a chance to drain the queue while the
/// cancellation flag still lets a retiring session stop immediately.
fn send_event(tx: &SyncSender<Event>, mut event: Event, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match tx.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(next)) => {
                event = next;
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// The session from setup to bus disconnect: subscribe, CreateSession,
/// BindShortcuts, ListShortcuts, then pump.
fn run(
    session_id: SessionId,
    preferred: &[(ShortcutId, String)],
    reconfigure: bool,
    tx: &SyncSender<Event>,
    cancel: &AtomicBool,
    connection_state: &Mutex<super::ConnectionState>,
    conn: Connection,
) -> Result<(), Why> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let sender = conn
        .unique_name()
        .map(|name| mangle_sender(name.as_str()))
        .ok_or_else(|| "the session bus issued no unique name".to_string())?;
    let proxy = shortcuts_proxy(&conn).map_err(|err| explain("GlobalShortcuts", err))?;
    let version = proxy
        .get_property::<u32>("version")
        .map_err(|err| explain("GlobalShortcuts.version", err))?;

    // Subscribe before setup. A shortcut can fire as soon as the bind
    // completes, before the pump loop starts.
    let signals = watch_signals(&conn)?;
    {
        let Ok(mut slot) = connection_state.lock() else {
            drop(signals.into_inner());
            return Err("could not lock shortcut session state".to_string().into());
        };
        if cancel.load(Ordering::Acquire) {
            drop(signals.into_inner());
            return Ok(());
        }
        slot.setup_signals = Some(signals);
    }
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }

    let session_token = handle_token();
    let created = request(&conn, &sender, "CreateSession", QUICK, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        options.insert("session_handle_token", Value::from(session_token.as_str()));
        proxy.call("CreateSession", &(options,))
    })?;
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    // `session_handle` has type `s` for historical reasons. The XML specifies
    // this type, so parse the value before passing it as a path.
    let session_path = created
        .get("session_handle")
        .and_then(|value| string_of(value))
        .ok_or_else(|| "CreateSession returned no session_handle".to_string())?;
    let session_object = ObjectPath::try_from(session_path.clone())
        .map_err(|err| format!("session_handle {session_path:?} is not a path: {err}"))?;
    let session = Session { conn: conn.clone(), path: session_path.clone() };
    let _ = send_event(
        tx,
        Event::Note {
            session: session_id,
            line: format!("trigger: {SHORTCUTS_INTERFACE} v{version} session {session_path}"),
        },
        cancel,
    );

    // Bind once per session.
    let bound = request(&conn, &sender, "BindShortcuts", BIND, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        // No parent window exists because the daemon has no modal surface.
        proxy.call("BindShortcuts", &(session_object.clone(), payload(preferred), "", options))
    })?;
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let allowed: Vec<ShortcutId> = preferred.iter().map(|(id, _)| *id).collect();
    let mut bindings = bound
        .get("shortcuts")
        .map(|value| bindings_from_value(value, &allowed))
        .unwrap_or_default();

    // An explicit Apply of shortcut settings asks the version-2 portal to show
    // its configuration UI. A user can cancel it; the confirmed Bind/List
    // state remains valid and is published without waiting for Changed.
    if reconfigure && version >= 2 {
        if let Err(why) = configure_shortcuts(&proxy, &session_object) {
            let _ = send_event(
                tx,
                Event::Note {
                    session: session_id,
                    line: format!("trigger: ConfigureShortcuts failed - {}", why.reason),
                },
                cancel,
            );
        }
    }

    // ListShortcuts returns the portal's current binding set. A portal can
    // preserve a previous user's key and ignore preferred_trigger, so this
    // result is authoritative even when it is empty.
    match request(&conn, &sender, "ListShortcuts", QUICK, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        proxy.call("ListShortcuts", &(session_object.clone(), options))
    }) {
        Ok(listed) => {
            bindings = listed
                .get("shortcuts")
                .map(|value| bindings_from_value(value, &allowed))
                .unwrap_or_default();
        }
        // A portal can bind shortcuts but fail to list them. This is not fatal;
        // the Bind result still identifies the accepted shortcuts.
        Err(why) => {
            let _ = send_event(
                tx,
                Event::Note {
                    session: session_id,
                    line: format!("trigger: ListShortcuts failed - {}", why.reason),
                },
                cancel,
            );
        }
    }

    if !send_event(
        tx,
        Event::Bound { session: session_id, bindings },
        cancel,
    ) {
        return Ok(());
    }

    let signals = connection_state.lock().ok().and_then(|mut slot| slot.setup_signals.take());
    let Some(signals) = signals else { return Ok(()) };
    match pump(signals, &session_path, session_id, &allowed, tx, cancel) {
        PumpEnd::StreamEnded if !cancel.load(Ordering::Acquire) => {
            let _ = send_event(
                tx,
                Event::Unavailable {
                    session: session_id,
                    reason: "GlobalShortcuts session ended".to_string(),
                    advice: None,
                },
                cancel,
            );
        }
        PumpEnd::ReceiverGone | PumpEnd::StreamEnded => {}
    }
    // Drop the session to call Close. A retiring caller closes the shared
    // connection first, so this call is harmless when the bus is already gone.
    drop(session);
    Ok(())
}

/// Ask a version-2 portal to show its native shortcut editor.
fn configure_shortcuts(proxy: &Proxy<'static>, session: &ObjectPath<'_>) -> Result<(), Why> {
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    proxy
        .call("ConfigureShortcuts", &(session.clone(), "", options))
        .map_err(|err| explain("ConfigureShortcuts", err))
}

/// Encode each shortcut's id, description, and preferred trigger.
fn payload(
    preferred: &[(ShortcutId, String)],
) -> Vec<(String, HashMap<&'static str, Value<'static>>)> {
    preferred
        .iter()
        .map(|(id, chord)| {
            let mut props: HashMap<&'static str, Value<'static>> = HashMap::new();
            props.insert("description", Value::from(id.description()));
            // The preferred trigger is optional. The user's binding takes
            // priority, and an implementation can ignore this key entirely.
            if !chord.is_empty() {
                props.insert("preferred_trigger", Value::from(chord.clone()));
            }
            (id.as_str().to_string(), props)
        })
        .collect()
}

enum PumpEnd {
    ReceiverGone,
    StreamEnded,
}

/// Convert shortcut signals to [`Event`] values until the connection ends.
fn pump(
    mut signals: MessageIterator,
    session: &str,
    session_id: SessionId,
    allowed: &[ShortcutId],
    tx: &SyncSender<Event>,
    cancel: &AtomicBool,
) -> PumpEnd {
    let outcome = loop {
        if cancel.load(Ordering::Acquire) {
            break PumpEnd::ReceiverGone;
        }
        let Some(message) = signals.next() else { break PumpEnd::StreamEnded };
        let Ok(message) = message else { continue };
        let header = message.header();
        let Some(member) = header.member() else { continue };
        let event = match member.as_str() {
            "Activated" => fired(&message, session, session_id, true, allowed),
            "Deactivated" => fired(&message, session, session_id, false, allowed),
            "ShortcutsChanged" => changed(&message, session, session_id, allowed),
            _ => None,
        };
        let Some(event) = event else { continue };
        if !send_event(tx, event, cancel) {
            break PumpEnd::ReceiverGone;
        }
    };
    drop(signals.into_inner());
    outcome
}

/// One `Activated`/`Deactivated` event (`osta{sv}`) for this session.
fn fired(
    message: &zbus::Message,
    session: &str,
    session_id: SessionId,
    activated: bool,
    allowed: &[ShortcutId],
) -> Option<Event> {
    let (path, id, _timestamp, _options) = message
        .body()
        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
        .ok()?;
    if path.as_str() != session {
        return None;
    }
    match ShortcutId::parse(&id) {
        Some(id) if allowed.contains(&id) => Some(Event::Fired { session: session_id, id, activated }),
        Some(_) => None,
        None => Some(Event::Note {
            session: session_id,
            line: format!("trigger: portal fired unknown shortcut {id:?}"),
        }),
    }
}

/// `ShortcutsChanged` (`oa(sa{sv})`) for this session.
///
/// Accept the specification's array-of-struct form and a dictionary form.
/// xdg-desktop-portal-hyprland declares the dictionary signature on its
/// implementation interface. The signal shape has changed across
/// implementations, and one fallback handles both forms.
fn changed(
    message: &zbus::Message,
    session: &str,
    session_id: SessionId,
    allowed: &[ShortcutId],
) -> Option<Event> {
    let body = message.body();
    if let Ok((path, shortcuts)) = body.deserialize::<(OwnedObjectPath, WirePairs)>() {
        if path.as_str() != session {
            return None;
        }
        return Some(Event::Changed {
            session: session_id,
            bindings: bindings_from_pairs(shortcuts, allowed),
        });
    }
    let (path, shortcuts) = body.deserialize::<(OwnedObjectPath, WireDict)>().ok()?;
    if path.as_str() != session {
        return None;
    }
    Some(Event::Changed {
        session: session_id,
        bindings: bindings_from_pairs(shortcuts.into_iter().collect(), allowed),
    })
}

/// The specification's `a(sa{sv})` form after deserialization.
type WirePairs = Vec<(String, HashMap<String, OwnedValue>)>;
/// The same data as a dictionary. The Hyprland backend declares this form
/// for `ShortcutsChanged`.
type WireDict = HashMap<String, HashMap<String, OwnedValue>>;

/// Build bindings from a typed `shortcuts` payload.
fn bindings_from_pairs(pairs: WirePairs, allowed: &[ShortcutId]) -> Vec<Binding> {
    bindings_of(
        pairs.into_iter().map(|(id, props)| {
            let trigger = props.get("trigger_description").and_then(|value| string_of(value));
            (id, trigger)
        }),
        allowed,
    )
}

/// Build bindings from the `shortcuts` value in a Response result map.
/// Each value arrives inside a variant.
fn bindings_from_value(value: &OwnedValue, allowed: &[ShortcutId]) -> Vec<Binding> {
    let entries: Vec<(String, Option<String>)> = match peel(value) {
        Value::Array(array) => array
            .iter()
            .filter_map(|entry| {
                let Value::Structure(fields) = peel(entry) else { return None };
                let mut fields = fields.fields().iter();
                let id = string_of(fields.next()?)?;
                let trigger = fields.next().and_then(|props| dict_string(props, "trigger_description"));
                Some((id, trigger))
            })
            .collect(),
        Value::Dict(dict) => dict
            .iter()
            .filter_map(|(key, props)| {
                let id = string_of(key)?;
                Some((id, dict_string(props, "trigger_description")))
            })
            .collect(),
        _ => Vec::new(),
    };
    bindings_of(entries, allowed)
}

/// Apply one rule to all entries. Keep only currently requested ids, at most
/// once each, in registration order. A blank `trigger_description` means
/// "bound, key unknown", not a key named "".
fn bindings_of(
    entries: impl IntoIterator<Item = (String, Option<String>)>,
    allowed: &[ShortcutId],
) -> Vec<Binding> {
    let mut found: Vec<(ShortcutId, Option<String>)> = Vec::with_capacity(allowed.len());
    for (id, trigger) in entries {
        let Some(id) = ShortcutId::parse(&id) else { continue };
        if !allowed.contains(&id) || found.iter().any(|(known, _)| *known == id) {
            continue;
        }
        found.push((id, trigger.filter(|text| !text.trim().is_empty())));
    }
    allowed
        .iter()
        .filter_map(|id| {
            found
                .iter()
                .find(|(known, _)| known == id)
                .map(|(_, trigger)| Binding { id: *id, trigger: trigger.clone() })
        })
        .collect()
}

/// Read one string key from an `a{sv}` value.
fn dict_string(value: &Value<'_>, key: &str) -> Option<String> {
    let Value::Dict(dict) = peel(value) else { return None };
    dict.iter()
        .find(|(name, _)| string_of(name).is_some_and(|name| name == key))
        .and_then(|(_, entry)| string_of(entry))
}

/// Call one portal method and map its response to the shortcuts error policy.
fn request(
    conn: &Connection,
    sender: &str,
    step: &'static str,
    budget: Duration,
    call: impl FnOnce(&str) -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, Why> {
    let deadline = Instant::now() + budget;
    let response = portal_request::request(conn, sender, step, deadline, call).map_err(|failure| {
        match failure {
            Failure::Call(error) => explain(step, error),
            Failure::TimedOut => format!("{step}: no answer within {}s", budget.as_secs()).into(),
            Failure::Protocol(detail) => detail.into(),
        }
    })?;
    match response.code {
        RESPONSE_SUCCESS => Ok(response.results),
        RESPONSE_CANCELLED => Err(format!("{step}: the user dismissed the shortcuts dialog").into()),
        code => Err(format!("{step}: the portal ended the request (code {code})").into()),
    }
}

/// Subscribe to every `GlobalShortcuts` signal on the portal object.
/// Register the rule before setup. The queue preserves a press during setup.
fn watch_signals(conn: &Connection) -> Result<MessageIterator, Why> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(PORTAL_BUS)
        .and_then(|builder| builder.path(PORTAL_PATH))
        .and_then(|builder| builder.interface(SHORTCUTS_INTERFACE))
        .map_err(|err| format!("bad match rule for {SHORTCUTS_INTERFACE}: {err}"))?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(SIGNAL_QUEUE))
        .map_err(|err| explain("GlobalShortcuts signals", err))
}

/// The portal session object. Drop it to call `Close`.
struct Session {
    conn: Connection,
    path: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        let proxy = Proxy::new_owned(
            self.conn.clone(),
            PORTAL_BUS.to_string(),
            self.path.clone(),
            SESSION_INTERFACE.to_string(),
        );
        if let Ok(proxy) = proxy {
            let _: zbus::Result<()> = proxy.call("Close", &());
        }
    }
}

/// The `GlobalShortcuts` proxy for the portal object.
fn shortcuts_proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        conn.clone(),
        PORTAL_BUS.to_string(),
        PORTAL_PATH.to_string(),
        SHORTCUTS_INTERFACE.to_string(),
    )
}

/// Turn a zbus failure into text that a user can act on.
///
/// The app-id refusal needs a launch method, not a setting. It is not a
/// missing feature or a denial. It means that this launch has no identity.
/// Other failures keep the portal's own words because a category would lose
/// useful detail.
fn explain(step: &str, err: zbus::Error) -> Why {
    match &err {
        zbus::Error::MethodError(name, detail, _) => {
            refusal(step, name.as_str(), detail.as_deref())
        }
        zbus::Error::InterfaceNotFound | zbus::Error::Address(_) => {
            Why::from(format!("{step}: no portal here ({err})"))
        }
        other => Why::from(format!("{step}: {other}")),
    }
}

/// Build one refused method call in user-facing text. Keep this logic
/// separate from [`explain`] because tests can pin the text without a live
/// `zbus::Error::MethodError` message.
fn refusal(step: &str, name: &str, detail: Option<&str>) -> Why {
    let text = detail.unwrap_or_default();
    if name.ends_with(".NotAllowed") && text.to_lowercase().contains("app id") {
        return Why {
            reason: format!("{step}: the portal requires an app id"),
            advice: Some(
                "xdg-desktop-portal names an app from the systemd unit a desktop-entry launch creates (app-chibipop-*.scope, with chibipop.desktop installed) and refuses shortcut sessions without one - launch chibipop from its desktop entry or autostart unit, or bind the control socket's `ctl trigger-down|trigger-up` verbs in your compositor instead (the settings window's hotkey section has the exact bind lines for this binary)"
                    .to_string(),
            ),
        };
    }
    Why::from(format!("{step}: {name}: {text}"))
}

/// Skip nested variants. Read the value by its type, not its wrapper.
fn peel<'a, 'v>(value: &'a Value<'v>) -> &'a Value<'v> {
    let mut current = value;
    while let Value::Value(inner) = current {
        current = inner;
    }
    current
}

/// Read an `s` value. Accept object paths because `session_handle` has type
/// `s` in the specification and portals send both forms.
fn string_of(value: &Value<'_>) -> Option<String> {
    match peel(value) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<ShortcutId> {
        ShortcutId::ALL.to_vec()
    }

    /// Build one `(sa{sv})` shortcut entry without a bus.
    fn shortcut(id: &str, trigger: Option<&str>) -> Value<'static> {
        let mut props: HashMap<String, Value<'static>> = HashMap::new();
        props.insert("description".to_string(), Value::from("whatever the dialog said"));
        if let Some(trigger) = trigger {
            props.insert("trigger_description".to_string(), Value::from(trigger.to_string()));
        }
        Value::from((id.to_string(), props))
    }

    fn shortcuts(entries: Vec<Value<'static>>) -> OwnedValue {
        OwnedValue::try_from(Value::from(entries)).expect("a test value is ownable")
    }

    #[test]
    fn the_spec_shape_parses_into_bindings() {
        let payload =
            shortcuts(vec![shortcut("trigger", Some("Alt+F")), shortcut("anki-add", Some("Alt+A"))]);
        assert_eq!(
            vec![
                Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
                Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
            ],
            bindings_from_value(&payload, &allowed())
        );
    }

    #[test]
    fn the_bindings_come_back_in_registration_order() {
        let payload =
            shortcuts(vec![shortcut("anki-add", Some("Alt+A")), shortcut("trigger", Some("Alt+F"))]);
        let ids: Vec<ShortcutId> = bindings_from_value(&payload, &allowed()).iter().map(|b| b.id).collect();
        assert_eq!(vec![ShortcutId::Trigger, ShortcutId::AnkiAdd], ids);
    }

    #[test]
    fn an_empty_trigger_description_is_bound_without_a_key() {
        let blank = shortcuts(vec![shortcut("trigger", Some(""))]);
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: None }],
            bindings_from_value(&blank, &allowed())
        );
        let missing = shortcuts(vec![shortcut("anki-add", None)]);
        assert_eq!(
            vec![Binding { id: ShortcutId::AnkiAdd, trigger: None }],
            bindings_from_value(&missing, &allowed())
        );
    }

    #[test]
    fn the_dict_shape_reads_the_same() {
        let mut props: HashMap<String, Value<'static>> = HashMap::new();
        props.insert("trigger_description".to_string(), Value::from("Meta+F"));
        let mut dict: HashMap<String, Value<'static>> = HashMap::new();
        dict.insert("trigger".to_string(), Value::from(props));
        let payload = OwnedValue::try_from(Value::from(dict)).expect("ownable");
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("Meta+F".into()) }],
            bindings_from_value(&payload, &allowed())
        );
    }

    #[test]
    fn foreign_and_repeated_ids_are_dropped() {
        let payload = shortcuts(vec![
            shortcut("trigger", Some("Alt+F")),
            shortcut("trigger", Some("Alt+G")),
            shortcut("", None),
        ]);
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) }],
            bindings_from_value(&payload, &[ShortcutId::Trigger, ShortcutId::AnkiAdd])
        );
    }

    #[test]
    fn a_nonsense_payload_is_no_bindings() {
        let number = OwnedValue::try_from(Value::U32(7)).expect("ownable");
        assert!(bindings_from_value(&number, &allowed()).is_empty());
        let text = OwnedValue::try_from(Value::from("shortcuts")).expect("ownable");
        assert!(bindings_from_value(&text, &allowed()).is_empty());
    }

    #[test]
    fn the_typed_signal_payload_parses_too() {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        props.insert(
            "trigger_description".to_string(),
            OwnedValue::try_from(Value::from("Alt+F")).expect("ownable"),
        );
        let pairs: WirePairs =
            vec![("trigger".to_string(), props), ("nope".to_string(), HashMap::new())];
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) }],
            bindings_from_pairs(pairs, &allowed())
        );
    }

    #[test]
    fn the_bind_payload_contains_every_requested_id() {
        let asked = vec![
            (ShortcutId::Trigger, "ALT+f".to_string()),
            (ShortcutId::AnkiAdd, "ALT+a".to_string()),
        ];
        let built = payload(&asked);
        assert_eq!(2, built.len());
        let ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(vec!["trigger", "anki-add"], ids);
        for (id, props) in &built {
            assert!(props.contains_key("description"), "{id} needs dialog text");
            assert!(props.contains_key("preferred_trigger"));
        }
    }

    #[test]
    fn an_empty_chord_sends_no_preferred_trigger() {
        let asked = [(ShortcutId::Trigger, String::new()), (ShortcutId::AnkiAdd, "ALT+a".to_string())];
        let built = payload(&asked);
        assert!(!built[0].1.contains_key("preferred_trigger"));
        assert!(built[1].1.contains_key("preferred_trigger"));
    }

    #[test]
    fn the_app_id_refusal_explains_the_launch_requirement() {
        let said = refusal(
            "CreateSession",
            "org.freedesktop.portal.Error.NotAllowed",
            Some("An app id is required"),
        );
        assert_eq!("CreateSession: the portal requires an app id", said.reason);
        let advice = said.advice.expect("the app-id case has a way out");
        assert!(advice.contains("chibipop.desktop"), "{advice}");
        assert!(advice.contains("ctl trigger-down|trigger-up"), "{advice}");
        assert!(!advice.contains("chibipop ctl"), "{advice}");
    }

    #[test]
    fn other_refusals_quote_the_portal() {
        let said = refusal(
            "BindShortcuts",
            "org.freedesktop.DBus.Error.AccessDenied",
            Some("Invalid session"),
        );
        assert!(said.reason.contains("AccessDenied"), "{}", said.reason);
        assert!(said.reason.contains("Invalid session"), "{}", said.reason);
        assert_eq!(None, said.advice);
        let bare = refusal("ListShortcuts", "org.freedesktop.DBus.Error.UnknownMethod", None);
        assert!(bare.reason.contains("ListShortcuts"), "{}", bare.reason);
        assert!(bare.reason.contains("UnknownMethod"), "{}", bare.reason);
    }

    #[test]
    fn a_different_denial_is_not_mistaken_for_the_app_id_case() {
        let said =
            refusal("BindShortcuts", "org.freedesktop.portal.Error.NotAllowed", Some("no thanks"));
        assert_eq!(None, said.advice);
        assert!(said.reason.contains("no thanks"), "{}", said.reason);
    }

    #[test]
    fn the_probe_answers_without_exploding() {
        let _ = probe();
    }
}
