//! `org.freedesktop.portal.GlobalShortcuts`: ADR-0003's rung 1 for the
//! trigger channel, on its own thread.
//!
//! **Why a thread and blocking zbus.** Same two reasons as the capture
//! portal ([`crate::capture::portal::dbus`]): ADR-0001 keeps the calloop
//! pump sync, so no async runtime may appear here, and zbus is already in
//! the tree with a blocking API that rides the executor ksni starts. The
//! session then lives for the whole run — unlike the capture handshake,
//! which is over once it has an fd — because `Activated`/`Deactivated`
//! keep arriving on it for as long as the user holds a key. So this is a
//! *long-lived* portal client: handshake, then a signal pump that turns
//! D-Bus messages into [`Event`]s on a calloop channel.
//!
//! **The Request race** is avoided exactly as the capture module does it:
//! every portal method answers twice (an `o` handle now, a
//! `Request.Response` later), the handle is predictable
//! (`…/request/<SENDER>/<handle_token>`), so the match rule goes on
//! *before* the call. A `BindShortcuts` that needs no dialog answers
//! immediately, and subscribing afterwards would lose that answer.
//!
//! **What this portal cannot do**, verified against the interface XML on
//! this machine (version 2) rather than assumed:
//!
//! * There is no restore token and no persist mode. `BindShortcuts` runs
//!   once per session — the spec says so in as many words: "An
//!   application can only attempt bind shortcuts of a session once" —
//!   and remembering the user's key is the portal's job, not ours.
//!   `ListShortcuts` is how a previous session's binding is read back:
//!   "otherwise returns the shortcuts that were successfully bound in a
//!   previous session by this application".
//! * `ConfigureShortcuts` (the desktop's own rebind dialog) arrived in
//!   version 2. Nothing here calls it: it needs a live session *and* a
//!   user-initiated moment, and the portal on the reference machine
//!   reports version 1, so the call would be an untested path pointed at
//!   an interface that is not there. The rebind path this tree surfaces
//!   is the one every implementation has — the dialog `BindShortcuts`
//!   itself raises, and the desktop's own shortcut settings — with
//!   `ShortcutsChanged` as how we hear the result.
//!
//! **An app id is mandatory.** xdg-desktop-portal's frontend refuses
//! `CreateSession` with `NotAllowed`/"An app id is required" when it
//! cannot name the calling app, and for a non-sandboxed process it
//! derives that name from the systemd user unit
//! (`app[-<launcher>]-<ApplicationID>-<RANDOM>.scope|.slice|.service`)
//! *and* requires a matching `<ApplicationID>.desktop` to exist. A
//! daemon started from a shell has neither, so this rung is unreachable
//! there however new the portal is. That is not a bug to route around; it
//! is the diagnostic in [`explain`], because the fix is a real user
//! action (launch from the desktop entry or the autostart unit) and the
//! control socket keeps carrying the trigger meanwhile.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use calloop::channel::SyncSender;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

use super::{Binding, Event, ShortcutId};

/// The portal's well-known bus name.
const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
/// The portal's single object path; every portal interface lives here.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The interface whose presence on the session bus is this rung's
/// capability probe (ADR-0003: advertised capability, never compositor
/// identity).
pub const SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Shared across all portal interfaces: where a method's deferred answer
/// arrives, and how a session is closed.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `Response` code 0: carried out. 1: the user cancelled. Anything else
/// ended some other way.
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// `CreateSession` and `ListShortcuts` involve no human, so they answer
/// at bus speed or something is wrong.
const QUICK: Duration = Duration::from_secs(10);
/// `BindShortcuts` "will typically result the portal presenting a dialog"
/// — on KDE that is a human reading a list of keys, so the budget is a
/// human's, not a bus's. Nothing waits on it: the control socket is
/// already serving and the pump never blocks (ADR-0001).
const BIND: Duration = Duration::from_secs(180);

/// How large a burst of shortcut signals may queue while the handshake
/// is still running. Presses are human-paced; this only has to cover the
/// gap between subscribing and reaching the pump loop.
const SIGNAL_QUEUE: usize = 64;

/// Is `org.freedesktop.portal.GlobalShortcuts` answering on the session
/// bus? Never an error: no bus and no portal are the same answer.
pub fn probe() -> bool {
    version().is_some()
}

/// The portal's negotiated interface version — the lower of frontend and
/// desktop implementation, so it answers the only question that matters:
/// what will the code handling our call understand.
pub fn version() -> Option<u32> {
    let conn = Connection::session().ok()?;
    // The cheapest question that proves the *interface* is there rather
    // than merely the bus name.
    shortcuts_proxy(&conn).ok()?.get_property::<u32>("version").ok()
}

/// Register the two ids and pump their signals until the process ends.
///
/// Everything the session learns — the bound set, every press and
/// release, every diagnostic, and its own failure — arrives on `tx` as an
/// [`Event`]. The thread is deliberately never joined: it owns nothing
/// the daemon needs back, and an exiting process closes the connection,
/// which is what tells the portal the session is gone.
pub fn spawn(preferred: [(ShortcutId, String); 2], tx: SyncSender<Event>) -> std::io::Result<()> {
    std::thread::Builder::new().name("chibipop-shortcuts".to_string()).spawn(move || {
        if let Err(why) = run(&preferred, &tx) {
            let _ = tx.send(Event::Unavailable { reason: why.reason, advice: why.advice });
        }
    })?;
    Ok(())
}

/// Why the rung is not serving.
///
/// Two fields, because two readers: a tray row wants one short clause
/// (ADR-0006), and the log wants the whole story. Keeping them apart is
/// what lets the app-id case explain a launch method without pasting a
/// paragraph into a menu.
pub struct Why {
    /// Short enough for a status row.
    pub reason: String,
    /// What to do about it, when there is something to do.
    pub advice: Option<String>,
}

impl From<String> for Why {
    fn from(reason: String) -> Why {
        Why { reason, advice: None }
    }
}

/// The session, from handshake to the end of the bus: subscribe,
/// CreateSession, BindShortcuts, ListShortcuts, then pump.
fn run(preferred: &[(ShortcutId, String); 2], tx: &SyncSender<Event>) -> Result<(), Why> {
    let conn = Connection::session().map_err(|err| format!("no session bus: {err}"))?;
    let sender = conn
        .unique_name()
        .map(|name| mangle_sender(name.as_str()))
        .ok_or_else(|| "the session bus issued no unique name".to_string())?;
    let proxy = shortcuts_proxy(&conn).map_err(|err| explain("GlobalShortcuts", err))?;
    let version = proxy
        .get_property::<u32>("version")
        .map_err(|err| explain("GlobalShortcuts.version", err))?;

    // Before the handshake: a shortcut can fire the instant the bind
    // lands, and the pump loop below is not running yet.
    let signals = watch_signals(&conn)?;

    let session_token = handle_token();
    let created = request(&conn, &sender, "CreateSession", QUICK, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        options.insert("session_handle_token", Value::from(session_token.as_str()));
        proxy.call("CreateSession", &(options,))
    })?;
    // `session_handle` is typed `s` by historical accident (the XML says
    // so out loud), so it has to be re-parsed as a path to be passed back.
    let session_path = created
        .get("session_handle")
        .and_then(|value| string_of(value))
        .ok_or_else(|| "CreateSession returned no session_handle".to_string())?;
    let session_object = ObjectPath::try_from(session_path.clone())
        .map_err(|err| format!("session_handle {session_path:?} is not a path: {err}"))?;
    let session = Session { conn: conn.clone(), path: session_path.clone() };
    let _ = tx.send(Event::Note(format!(
        "trigger: {SHORTCUTS_INTERFACE} v{version} session {session_path}"
    )));

    // -- BindShortcuts: once per session, exactly two ids --
    let bound = request(&conn, &sender, "BindShortcuts", BIND, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        // No parent window: the daemon has no surface to be modal over.
        proxy.call("BindShortcuts", &(session_object.clone(), payload(preferred), "", options))
    })?;
    let mut bindings = bound.get("shortcuts").map(bindings_from_value).unwrap_or_default();

    // -- ListShortcuts: the portal's own account of what is bound now,
    // which is what a status row should quote. `BindShortcuts` answers
    // with the subset it accepted; this answers with the session's whole
    // truth, and on an implementation that remembers a previous session
    // it is where a trigger description comes from at all.
    match request(&conn, &sender, "ListShortcuts", QUICK, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        proxy.call("ListShortcuts", &(session_object.clone(), options))
    }) {
        Ok(listed) => {
            let listed = listed.get("shortcuts").map(bindings_from_value).unwrap_or_default();
            if !listed.is_empty() {
                bindings = listed;
            }
        }
        // A portal that binds but cannot list is odd, not fatal: the bind
        // result already said what we have.
        Err(why) => {
            let _ = tx.send(Event::Note(format!(
                "trigger: ListShortcuts failed - {}",
                why.reason
            )));
        }
    }

    if tx.send(Event::Bound(bindings)).is_err() {
        return Ok(());
    }

    pump(signals, &session_path, tx);
    // The pump only ends when the bus does, or when the pump went away.
    // Closing the session on the way out is the honest way to drop it.
    drop(session);
    Ok(())
}

/// The two shortcuts as `a(sa{sv})`: id, description, preferred trigger.
///
/// Built from a fixed-size input, so the wire payload cannot grow a third
/// id without a type change.
fn payload(
    preferred: &[(ShortcutId, String); 2],
) -> Vec<(String, HashMap<&'static str, Value<'static>>)> {
    preferred
        .iter()
        .map(|(id, chord)| {
            let mut props: HashMap<&'static str, Value<'static>> = HashMap::new();
            props.insert("description", Value::from(id.description()));
            // Optional, and a *preference*: the user's own binding wins,
            // and an implementation is free to ignore it outright
            // (xdg-desktop-portal-hyprland does, logging it as an unknown
            // key), which is why nothing downstream reads it back as the
            // current key.
            if !chord.is_empty() {
                props.insert("preferred_trigger", Value::from(chord.clone()));
            }
            (id.as_str().to_string(), props)
        })
        .collect()
}

/// Turn shortcut signals into [`Event`]s until the connection ends.
fn pump(signals: MessageIterator, session: &str, tx: &SyncSender<Event>) {
    for message in signals {
        let Ok(message) = message else { continue };
        let header = message.header();
        let Some(member) = header.member() else { continue };
        let event = match member.as_str() {
            "Activated" => fired(&message, session, true),
            "Deactivated" => fired(&message, session, false),
            "ShortcutsChanged" => changed(&message, session),
            _ => None,
        };
        let Some(event) = event else { continue };
        if tx.send(event).is_err() {
            // The pump is gone: so is the reason to hold the session.
            return;
        }
    }
}

/// One `Activated`/`Deactivated` (`osta{sv}`) for our own session.
fn fired(message: &zbus::Message, session: &str, activated: bool) -> Option<Event> {
    let (path, id, _timestamp, _options) = message
        .body()
        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
        .ok()?;
    if path.as_str() != session {
        return None;
    }
    // An id we never registered is not ours to act on. It is worth a
    // line, though: it means something else is bound under our name.
    match ShortcutId::parse(&id) {
        Some(id) => Some(Event::Fired { id, activated }),
        None => Some(Event::Note(format!("trigger: portal fired unknown shortcut {id:?}"))),
    }
}

/// `ShortcutsChanged` (`oa(sa{sv})`) for our own session.
///
/// Both the spec's array-of-struct and a dict `a{sa{sv}}` are accepted.
/// That is not defensive padding:
/// xdg-desktop-portal-hyprland declares this signal with the dict
/// signature on its implementation interface, so the shape has already
/// diverged once in the wild and reading either costs one fallback.
fn changed(message: &zbus::Message, session: &str) -> Option<Event> {
    let body = message.body();
    if let Ok((path, shortcuts)) = body.deserialize::<(OwnedObjectPath, WirePairs)>() {
        if path.as_str() != session {
            return None;
        }
        return Some(Event::Changed(bindings_from_pairs(shortcuts)));
    }
    let (path, shortcuts) = body.deserialize::<(OwnedObjectPath, WireDict)>().ok()?;
    if path.as_str() != session {
        return None;
    }
    Some(Event::Changed(bindings_from_pairs(shortcuts.into_iter().collect())))
}

/// The spec's `a(sa{sv})`, deserialized.
type WirePairs = Vec<(String, HashMap<String, OwnedValue>)>;
/// The same information as a dict, which is how the hyprland backend
/// declares `ShortcutsChanged`.
type WireDict = HashMap<String, HashMap<String, OwnedValue>>;

/// Bindings from a typed `shortcuts` payload.
fn bindings_from_pairs(pairs: WirePairs) -> Vec<Binding> {
    bindings_of(pairs.into_iter().map(|(id, props)| {
        let trigger = props.get("trigger_description").and_then(|value| string_of(value));
        (id, trigger)
    }))
}

/// Bindings from a `shortcuts` value inside a `Response` result map,
/// where everything arrives boxed as a variant.
fn bindings_from_value(value: &OwnedValue) -> Vec<Binding> {
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
    bindings_of(entries)
}

/// The shared rule: only our own ids, at most once each, in
/// [`ShortcutId::ALL`] order — so a status row reads the same however a
/// portal happened to order its answer — and a blank
/// `trigger_description` is "bound, key unknown" rather than a key named
/// "".
fn bindings_of(entries: impl IntoIterator<Item = (String, Option<String>)>) -> Vec<Binding> {
    let mut found: Vec<(ShortcutId, Option<String>)> = Vec::with_capacity(ShortcutId::ALL.len());
    for (id, trigger) in entries {
        let Some(id) = ShortcutId::parse(&id) else { continue };
        if found.iter().any(|(known, _)| *known == id) {
            continue;
        }
        found.push((id, trigger.filter(|text| !text.trim().is_empty())));
    }
    ShortcutId::ALL
        .into_iter()
        .filter_map(|id| {
            found
                .iter()
                .find(|(known, _)| *known == id)
                .map(|(_, trigger)| Binding { id, trigger: trigger.clone() })
        })
        .collect()
}

/// One string key out of an `a{sv}` value.
fn dict_string(value: &Value<'_>, key: &str) -> Option<String> {
    let Value::Dict(dict) = peel(value) else { return None };
    dict.iter()
        .find(|(name, _)| string_of(name).is_some_and(|name| name == key))
        .and_then(|(_, entry)| string_of(entry))
}

/// One portal method call, subscription first: predict the Request path,
/// register the match rule, *then* call, then wait for `Response`.
fn request(
    conn: &Connection,
    sender: &str,
    step: &'static str,
    budget: Duration,
    call: impl FnOnce(&str) -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, Why> {
    let deadline = Instant::now() + budget;
    let token = handle_token();
    let predicted = request_path(sender, &token);
    let watch = watch_response(conn, &predicted, step)?;

    let handle = call(&token).map_err(|err| explain(step, err))?;
    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // A portal that ignored `handle_token`: listen where the handle
        // actually is, and accept that a very fast reply may be lost —
        // the deadline is what keeps that from hanging.
        drop(watch);
        watch_response(conn, handle.as_str(), step)?
    };
    Ok(watch.wait(step, deadline, budget)?)
}

/// A `Request.Response` payload: the spec's `(ua{sv})`, or why we never
/// got one.
type Answer = Result<(u32, HashMap<String, OwnedValue>), String>;

/// A registered subscription to one Request's `Response`, already being
/// pumped by its own thread.
///
/// zbus's blocking iterator has no bounded wait, and an unanswered dialog
/// must not wedge this thread forever, so the iterator goes to a
/// short-lived thread and the caller uses `recv_timeout`.
struct ResponseWatch {
    rx: Receiver<Answer>,
}

fn watch_response(
    conn: &Connection,
    path: &str,
    step: &'static str,
) -> Result<ResponseWatch, Why> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(PORTAL_BUS)
        .and_then(|builder| builder.path(path.to_string()))
        .and_then(|builder| builder.interface(REQUEST_INTERFACE))
        .and_then(|builder| builder.member("Response"))
        .map_err(|err| format!("{step}: bad match rule for {path}: {err}"))?
        .build();
    // One Response per Request: the queue only has to outlive the gap
    // between registering and reading.
    let iterator =
        MessageIterator::for_match_rule(rule, conn, Some(2)).map_err(|err| explain(step, err))?;

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("chibipop-shortcuts-req".to_string())
        .spawn(move || {
            let answer = match iterator.into_iter().next() {
                Some(Ok(message)) => message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map_err(|err| format!("{step}: malformed Response: {err}")),
                Some(Err(err)) => Err(format!("{step}: bus error waiting: {err}")),
                // The iterator ended: the connection closed under us.
                None => {
                    Err(format!("{step}: the session bus closed before the portal answered"))
                }
            };
            let _ = tx.send(answer);
        })
        .map_err(|err| format!("{step}: no thread for the wait: {err}"))?;

    Ok(ResponseWatch { rx })
}

impl ResponseWatch {
    /// Block until the portal answers or `deadline` passes. `budget` is
    /// only for the message: a timeout has to say how long it waited, or
    /// the log cannot tell a slow bus from an unanswered dialog.
    fn wait(
        self,
        step: &'static str,
        deadline: Instant,
        budget: Duration,
    ) -> Result<HashMap<String, OwnedValue>, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(remaining) {
            Ok(Ok((RESPONSE_SUCCESS, results))) => Ok(results),
            Ok(Ok((RESPONSE_CANCELLED, _))) => {
                Err(format!("{step}: the user dismissed the shortcuts dialog"))
            }
            Ok(Ok((code, _))) => Err(format!("{step}: the portal ended the request (code {code})")),
            Ok(Err(err)) => Err(err),
            Err(RecvTimeoutError::Timeout) => {
                Err(format!("{step}: no answer within {}s", budget.as_secs()))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(format!("{step}: the waiting thread stopped without answering"))
            }
        }
    }
}

/// Every `GlobalShortcuts` signal on the portal's object, on one match
/// rule. Registered before the handshake, so a press that lands during it
/// is queued rather than lost.
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

/// The portal session object. Dropping it calls `Close`.
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

/// The GlobalShortcuts proxy on the portal's single object.
fn shortcuts_proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        conn.clone(),
        PORTAL_BUS.to_string(),
        PORTAL_PATH.to_string(),
        SHORTCUTS_INTERFACE.to_string(),
    )
}

/// Our unique bus name as an object-path element: leading `:` dropped,
/// every `.` an `_`, exactly as the Request documentation specifies.
fn mangle_sender(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

/// Where the portal will put the Request object for `token`.
fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// A fresh `handle_token`: a valid object-path element, unique within
/// this process by the counter and unguessable enough by the clock.
fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chibipop_{now:x}_{n}")
}

/// A zbus failure as a sentence a user can act on.
///
/// The one case worth naming specially is the app-id refusal: it is not a
/// missing feature and not a denial, it is "this launch has no identity",
/// and the fix is a launch method rather than a setting. Everything else
/// keeps the portal's own words, which is more useful than a category.
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

/// One refused method call, in words. Split from [`explain`] because
/// this is the part worth pinning: a `zbus::Error::MethodError` cannot
/// be built without a live `Message`, and the sentence a user reads must
/// not go untested for want of a bus.
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

/// A variant may itself hold a variant; look through those wrappers so a
/// value is read by its type, not by how it was boxed.
fn peel<'a, 'v>(value: &'a Value<'v>) -> &'a Value<'v> {
    let mut current = value;
    while let Value::Value(inner) = current {
        current = inner;
    }
    current
}

/// An `s`. Object paths are accepted too: `session_handle` is specified
/// as `s` but reads as a path, and portals have shipped both.
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

    /// One `(sa{sv})` shortcut entry the way a portal sends one, without
    /// a bus.
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

    /// The spec's `a(sa{sv})`, which is what the frontend emits: both ids
    /// with the portal's own spelling of their keys.
    #[test]
    fn the_spec_shape_parses_into_both_bindings() {
        let payload =
            shortcuts(vec![shortcut("trigger", Some("Alt+F")), shortcut("anki-add", Some("Alt+A"))]);
        assert_eq!(
            vec![
                Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
                Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
            ],
            bindings_from_value(&payload)
        );
    }

    /// A status row must read the same however the portal ordered its
    /// answer.
    #[test]
    fn the_bindings_come_back_in_registration_order() {
        let payload =
            shortcuts(vec![shortcut("anki-add", Some("Alt+A")), shortcut("trigger", Some("Alt+F"))]);
        let ids: Vec<ShortcutId> = bindings_from_value(&payload).iter().map(|b| b.id).collect();
        assert_eq!(vec![ShortcutId::Trigger, ShortcutId::AnkiAdd], ids);
    }

    /// xdg-desktop-portal-hyprland answers `trigger_description: ""`,
    /// because on Hyprland the key lives in the compositor's config.
    /// Bound-but-unnamed must not read as unbound.
    #[test]
    fn an_empty_trigger_description_is_bound_without_a_key() {
        let blank = shortcuts(vec![shortcut("trigger", Some(""))]);
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: None }],
            bindings_from_value(&blank)
        );
        let missing = shortcuts(vec![shortcut("anki-add", None)]);
        assert_eq!(
            vec![Binding { id: ShortcutId::AnkiAdd, trigger: None }],
            bindings_from_value(&missing)
        );
    }

    /// The dict shape the hyprland backend declares for
    /// `ShortcutsChanged` reads the same as the spec's array.
    #[test]
    fn the_dict_shape_reads_the_same() {
        let mut props: HashMap<String, Value<'static>> = HashMap::new();
        props.insert("trigger_description".to_string(), Value::from("Meta+F"));
        let mut dict: HashMap<String, Value<'static>> = HashMap::new();
        dict.insert("trigger".to_string(), Value::from(props));
        let payload = OwnedValue::try_from(Value::from(dict)).expect("ownable");
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("Meta+F".into()) }],
            bindings_from_value(&payload)
        );
    }

    /// Only our own ids are ever acted on, and a duplicate cannot make
    /// the set grow: whatever a portal sends back, the daemon's view
    /// stays "at most the two shortcuts".
    #[test]
    fn foreign_and_repeated_ids_are_dropped() {
        let payload = shortcuts(vec![
            shortcut("trigger", Some("Alt+F")),
            shortcut("trigger", Some("Alt+G")),
            shortcut("screenshot", Some("Print")),
            shortcut("", None),
        ]);
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) }],
            bindings_from_value(&payload)
        );
    }

    /// A payload that is not a shortcut list at all is an empty answer,
    /// never a panic: it arrives from another process.
    #[test]
    fn a_nonsense_payload_is_no_bindings() {
        let number = OwnedValue::try_from(Value::U32(7)).expect("ownable");
        assert!(bindings_from_value(&number).is_empty());
        let text = OwnedValue::try_from(Value::from("shortcuts")).expect("ownable");
        assert!(bindings_from_value(&text).is_empty());
    }

    /// The typed path a signal takes reaches the same bindings as the
    /// boxed path a method result takes.
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
            bindings_from_pairs(pairs)
        );
    }

    /// The wire payload is exactly the two ids, each with a description,
    /// carrying the preferred trigger the config asked for.
    #[test]
    fn the_bind_payload_is_exactly_the_two_ids() {
        let asked = [
            (ShortcutId::Trigger, "ALT+f".to_string()),
            (ShortcutId::AnkiAdd, "ALT+a".to_string()),
        ];
        let built = payload(&asked);
        assert_eq!(2, built.len());
        let ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(vec!["trigger", "anki-add"], ids);
        for (id, props) in &built {
            assert!(props.contains_key("description"), "{id} needs dialog text");
            let want = if id == "trigger" { "ALT+f" } else { "ALT+a" };
            assert_eq!(Some(&Value::from(want.to_string())), props.get("preferred_trigger"));
        }
    }

    /// A chord the user cleared must not become an empty
    /// `preferred_trigger`: the key is optional, and omitting it lets the
    /// portal pick, while sending "" is a shortcut nobody can press.
    #[test]
    fn an_empty_chord_sends_no_preferred_trigger() {
        let asked =
            [(ShortcutId::Trigger, String::new()), (ShortcutId::AnkiAdd, "ALT+a".to_string())];
        let built = payload(&asked);
        assert!(!built[0].1.contains_key("preferred_trigger"));
        assert!(built[1].1.contains_key("preferred_trigger"));
    }

    /// The app-id refusal is the one failure a user can fix, so it says
    /// how — in the advice, where there is room, while the row-sized
    /// reason stays one clause. This is the live case on the reference
    /// machine: a daemon launched from a shell has no systemd app unit,
    /// so xdg-desktop-portal will not open a shortcuts session for it at
    /// all.
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
        // The way out must not spell a command that assumes `chibipop`
        // is on PATH: under `cargo run` it is not (ticket 51), and the
        // bind lines that name the running exe live in the window.
        assert!(!advice.contains("chibipop ctl"), "{advice}");
    }

    /// Any other refusal keeps the portal's own words: a category would
    /// throw away the only information in it, and inventing advice for a
    /// reason we do not understand would be worse than none.
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
        // A refusal with no message at all is still a sentence naming
        // the step that failed.
        let bare = refusal("ListShortcuts", "org.freedesktop.DBus.Error.UnknownMethod", None);
        assert!(bare.reason.contains("ListShortcuts"), "{}", bare.reason);
        assert!(bare.reason.contains("UnknownMethod"), "{}", bare.reason);
    }

    /// A `NotAllowed` that is not about an app id must not be dressed up
    /// as one: the launch advice would be wrong and the real reason lost.
    #[test]
    fn a_different_denial_is_not_mistaken_for_the_app_id_case() {
        let said =
            refusal("BindShortcuts", "org.freedesktop.portal.Error.NotAllowed", Some("no thanks"));
        assert_eq!(None, said.advice);
        assert!(said.reason.contains("no thanks"), "{}", said.reason);
    }

    // -- the predicted Request path (the race avoidance) --

    #[test]
    fn a_unique_bus_name_becomes_a_path_element() {
        assert_eq!("1_234", mangle_sender(":1.234"));
        assert_eq!("1_2_345", mangle_sender(":1.2.345"));
    }

    #[test]
    fn a_predicted_request_path_sits_under_the_portal_object() {
        assert_eq!(
            "/org/freedesktop/portal/desktop/request/1_234/chibipop_9_0",
            request_path("1_234", "chibipop_9_0")
        );
    }

    /// A token that is not a valid object-path element makes the portal
    /// reject the call outright, so the alphabet is part of the contract.
    #[test]
    fn a_handle_token_is_a_valid_path_element() {
        let token = handle_token();
        assert!(
            token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "token {token:?} must match [A-Za-z0-9_]+"
        );
        assert_ne!(token, handle_token());
    }

    /// The probe is a question about this machine, not an assertion: it
    /// must answer either way, with or without a bus.
    #[test]
    fn the_probe_answers_without_exploding() {
        let _ = probe();
    }
}
