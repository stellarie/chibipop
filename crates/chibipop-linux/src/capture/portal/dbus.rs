//! The `org.freedesktop.portal.ScreenCast` handshake: ADR-0002's
//! fallback capture rung, from first dialog to a PipeWire remote fd.
//!
//! **Why eager.** The portal is the one rung that asks a human for
//! permission, and a background daemon cannot ask politely twice. So
//! consent is taken *once, at startup*, in a single dialog covering
//! every monitor (`multiple = true`), rather than lazily on the first
//! hover — a permission dialog appearing the instant a tooltip should
//! have appeared is the worst moment chibipop could pick. `Start` hands
//! back a restore token, [`super::token`] persists it, and every later
//! launch replays it and sees no dialog at all.
//!
//! **Why blocking zbus and not `ashpd`.** `ashpd` is the ergonomic
//! choice and it is async-first: using it would drag a second async
//! runtime into the daemon, which ADR-0001 forbids — the calloop pump is
//! sync and stays sync. zbus is already in this tree (ksni pulls it, and
//! ksni's `async-io` feature resolves the very same version), so its
//! blocking API costs nothing new and rides the one `async-io` executor
//! ksni already starts. Everything here therefore runs on whatever
//! thread the caller provides; the lead gives it a dedicated portal
//! thread, and the daemon's loop never blocks on a dialog.
//!
//! **The Request race, and how it is avoided.** Every portal method
//! answers twice: the method reply carries an `o` request handle, and
//! the real answer arrives later as `Response` (`(ua{sv})`) on that
//! object. Subscribing after the method returns can miss a fast reply
//! outright — a portal that restores a session from a token answers
//! without ever drawing a dialog. xdg-desktop-portal fixed this by
//! making the handle *predictable*:
//! `/org/freedesktop/portal/desktop/request/<SENDER>/<handle_token>`,
//! where `<SENDER>` is our unique bus name minus the leading `:` with
//! every `.` turned into `_`. So each call here registers its match rule
//! at the predicted path *before* issuing the call, then checks that the
//! handle it got back is the path it guessed, and re-subscribes at the
//! returned path if some older portal disagreed.
//!
//! **Why a waiter thread.** zbus's blocking signal iterator has no
//! bounded wait, and the timeout here is a *total* budget across the
//! whole handshake (an unanswered dialog must not wedge a startup
//! forever). So each wait hands its iterator to a short-lived thread
//! that pushes the first `Response` down an `mpsc` channel, and the
//! caller uses `recv_timeout`. Abandoning the handshake closes the
//! shared connection, which ends those iterators and reaps the threads —
//! and is also the honest way to drop a half-open session.
//!
//! Signatures here were verified against
//! `/usr/share/dbus-1/interfaces/org.freedesktop.portal.ScreenCast.xml`
//! (interface version 6) and `org.freedesktop.portal.Request.xml` on
//! this machine, not from memory.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

/// The portal's well-known bus name.
pub const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
/// The portal's single object path; every portal interface lives here.
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The screen-cast interface this module speaks.
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";

/// Source types the portal offers (`SelectSources` `types`).
pub const SOURCE_MONITOR: u32 = 1;
/// Cursor modes (`AvailableCursorModes` / `SelectSources` `cursor_mode`).
/// `SPA`'s third mode, EMBEDDED (2), is deliberately absent: it
/// composites a pointer into the pixels we are about to OCR, and
/// ADR-0003's rung wants coordinates instead.
pub const CURSOR_MODE_HIDDEN: u32 = 1;
/// The cursor rides beside the pixels as stream metadata - ADR-0003's
/// rung-2 cursor source.
pub const CURSOR_MODE_METADATA: u32 = 4;
/// `persist_mode`: 2 = persist until explicitly revoked (ADR-0002).
pub const PERSIST_UNTIL_REVOKED: u32 = 2;
/// `persist_mode` and `restore_token` arrived in ScreenCast version 4.
///
/// A version-3 portal - xdg-desktop-portal-hyprland, at the time of
/// writing - cannot remember a grant at all, so ADR-0002's "silent
/// launches after" is unreachable there and every launch shows the
/// dialog. Sending the keys anyway would be harmless, because a portal
/// ignores options it does not know, but it would leave the missing
/// token unexplained; and the difference between "we asked and it
/// refused" and "it cannot" is the difference between a bug report and
/// a fact.
pub const PERSIST_MIN_VERSION: u32 = 4;

/// Shared across all portal interfaces: where a method's deferred answer
/// arrives.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
/// Shared across all portal interfaces: how a session is torn down.
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `Response` code 0: the request was carried out.
const RESPONSE_SUCCESS: u32 = 0;
/// `Response` code 1: the user cancelled the interaction.
const RESPONSE_CANCELLED: u32 = 1;
/// `Response` code 2: the interaction ended some other way - which is
/// also what a stale restore token looks like from out here.
const RESPONSE_ENDED: u32 = 2;

/// One monitor stream the portal handed back from `Start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// The PipeWire node to connect to.
    pub node_id: u32,
    /// The `position` property (logical layout coords), when sent.
    pub position: Option<(i32, i32)>,
    /// The `size` property (logical), when sent.
    pub size: Option<(i32, i32)>,
    /// The `source_type` property, when sent.
    pub source_type: Option<u32>,
}

/// Why the portal rung is not serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalError {
    /// No portal on the bus, or no ScreenCast interface on it.
    Absent(String),
    /// The user said no (Request response code 1).
    Denied,
    /// The portal ended the interaction itself (response code 2), or a
    /// stale restore token was rejected.
    Ended(String),
    /// The handshake did not finish inside the deadline.
    TimedOut(String),
    /// Anything the portal did that the spec does not describe.
    Protocol(String),
}

impl PortalError {
    /// The tray/settings row text: short, honest, and naming a way back
    /// that exists (e.g. "screen-capture permission denied - retry with
    /// Apply in the settings window or `chibipop ctl reload`").
    pub fn detail(&self) -> String {
        match self {
            PortalError::Absent(what) => format!(
                "no screen-capture portal on the session bus ({what}) - install xdg-desktop-portal \
                 and its compositor backend, then retry with `chibipop ctl reload`"
            ),
            // "From the tray" would be a lie: the tray's rows are
            // status, not buttons (ADR-0006), and the retry hook is the
            // `reload` verb - which is exactly what the settings
            // window's Apply sends. Both routes are named because a
            // stock-GNOME session has no tray to reach either from.
            PortalError::Denied => "screen-capture permission denied - retry with Apply in the \
                                    settings window or `chibipop ctl reload`"
                .to_string(),
            PortalError::Ended(why) => format!(
                "the portal ended the screen-capture request ({why}) - retry with Apply in the \
                 settings window or `chibipop ctl reload`"
            ),
            PortalError::TimedOut(step) => format!(
                "the screen-capture dialog went unanswered at {step} - answer it, then retry with \
                 `chibipop ctl reload`"
            ),
            PortalError::Protocol(what) => format!(
                "the screen-capture portal answered off-spec ({what}) - see the log; retry with \
                 `chibipop ctl reload`"
            ),
        }
    }

    /// Whether retrying with the SAME stored restore token can help.
    /// A denial cannot; a timeout can; a rejected token means the
    /// caller must drop the token and prompt again.
    pub fn retry_needs_fresh_consent(&self) -> bool {
        match self {
            // The user's answer was "no", and a token cannot argue.
            PortalError::Denied => true,
            // Code 2 is also how a stale or revoked token reads: the
            // portal closed the interaction rather than restoring.
            PortalError::Ended(_) => true,
            // A portal that is not there yet, a dialog nobody got to in
            // time, and a portal bug all leave the grant untouched.
            PortalError::Absent(_) | PortalError::TimedOut(_) | PortalError::Protocol(_) => false,
        }
    }
}

impl std::fmt::Display for PortalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for PortalError {}

/// A live ScreenCast session plus everything `Start` produced.
pub struct Consent {
    /// One entry per monitor the user shared, in the portal's order.
    pub streams: Vec<StreamInfo>,
    /// The token to persist for the next launch. `None` when the
    /// portal declined to issue one - or could not, see
    /// [`Consent::persists`].
    pub restore_token: Option<String>,
    /// This portal's ScreenCast interface version.
    pub version: u32,
    /// The portal is new enough to remember a grant
    /// ([`PERSIST_MIN_VERSION`]), so a missing `restore_token` means
    /// it declined rather than that it never could.
    pub persists: bool,
    /// The PipeWire remote from `OpenPipeWireRemote`.
    pub pipewire_fd: OwnedFd,
    /// Kept alive: closing it revokes the session, so the caller holds
    /// it for as long as it wants frames.
    pub session: Session,
}

/// The portal session object. Dropping it calls `Close`.
pub struct Session {
    /// The same connection the handshake ran on: `Close` must come from
    /// the peer that owns the session.
    conn: Connection,
    /// The session's object path, from `CreateSession`'s results.
    path: String,
}

impl Session {
    /// The session's object path, as the portal reported it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Explicitly close; `Drop` does this too, and ignores errors.
    pub fn close(&self) {
        let Ok(proxy) = Proxy::new(&self.conn, PORTAL_BUS, self.path.as_str(), SESSION_INTERFACE)
        else {
            return;
        };
        // A portal that already tore the session down, a bus that went
        // away, a second `close` - all of it is fine. There is nothing
        // to recover here and nothing worth logging at exit.
        let _: zbus::Result<()> = proxy.call("Close", &());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// Is `org.freedesktop.portal.ScreenCast` answering on the session bus?
/// Never an error: no bus and no portal are the same answer here.
pub fn probe() -> bool {
    let Ok(conn) = Connection::session() else {
        return false;
    };
    let Ok(proxy) = screencast_proxy(&conn) else {
        return false;
    };
    // The cheapest question that proves the *interface* is there, not
    // merely the bus name: an activatable name owner with no ScreenCast
    // implementation fails this.
    proxy.get_property::<u32>("version").is_ok()
}

/// The portal's advertised cursor modes, for the ADR-0003 rung-2
/// capability check. `None` when the property cannot be read.
pub fn available_cursor_modes() -> Option<u32> {
    let conn = Connection::session().ok()?;
    screencast_proxy(&conn).ok()?.get_property::<u32>("AvailableCursorModes").ok()
}

/// ADR-0002's eager startup consent, start to finish: CreateSession,
/// SelectSources (all monitors in ONE dialog), Start, and
/// OpenPipeWireRemote. Blocks the calling thread up to `timeout` in
/// total. `restore_token` is the previous run's token, which is what
/// makes the second launch silent.
pub fn open(
    restore_token: Option<&str>,
    cursor_metadata: bool,
    timeout: Duration,
) -> Result<Consent, PortalError> {
    let deadline = Instant::now() + timeout;
    let conn = Connection::session()
        .map_err(|err| PortalError::Absent(format!("no session bus: {err}")))?;

    match handshake(&conn, restore_token, cursor_metadata, deadline) {
        Ok(consent) => Ok(consent),
        Err(err) => {
            // Abandoning the handshake: close the connection rather than
            // leave a half-consented session up. It also ends any waiter
            // thread still parked on a signal iterator (see module docs).
            let _ = conn.close();
            Err(err)
        }
    }
}

/// The four calls, in the only order the portal accepts.
fn handshake(
    conn: &Connection,
    restore_token: Option<&str>,
    cursor_metadata: bool,
    deadline: Instant,
) -> Result<Consent, PortalError> {
    let sender = conn
        .unique_name()
        .map(|name| mangle_sender(name.as_str()))
        .ok_or_else(|| PortalError::Absent("the session bus issued no unique name".to_string()))?;
    let screencast = screencast_proxy(conn).map_err(|err| classify("ScreenCast", err))?;
    // Prove the interface exists before opening a dialog-shaped hole in
    // the startup budget: absence is a rung the ladder walks past.
    let version = screencast
        .get_property::<u32>("version")
        .map_err(|err| classify("ScreenCast.version", err))?;
    let persists = persists(version);
    let cursor =
        cursor_mode(screencast.get_property::<u32>("AvailableCursorModes").ok(), cursor_metadata);

    // -- CreateSession --
    let session_token = handle_token();
    let created = request(conn, &sender, "CreateSession", deadline, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        options.insert("session_handle_token", Value::from(session_token.as_str()));
        screencast.call("CreateSession", &(options,))
    })?;
    let session_path = created
        .get("session_handle")
        .and_then(|value| string_of(value))
        .ok_or_else(|| {
            PortalError::Protocol("CreateSession returned no session_handle".to_string())
        })?;
    // The spec types `session_handle` as `s` by historical accident, so
    // it has to be re-parsed as a path before it can be passed back.
    let session_object = ObjectPath::try_from(session_path.clone()).map_err(|err| {
        PortalError::Protocol(format!("session_handle {session_path:?} is not a path: {err}"))
    })?;
    // From here on a failure owes the portal a `Close`; the caller's
    // connection teardown in `open` is what delivers it.

    // -- SelectSources: one dialog, every monitor (ADR-0002) --
    request(conn, &sender, "SelectSources", deadline, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        options.insert("types", Value::U32(SOURCE_MONITOR));
        options.insert("multiple", Value::Bool(true));
        // v4-only keys, sent only to a portal that has them.
        if persists {
            options.insert("persist_mode", Value::U32(PERSIST_UNTIL_REVOKED));
            if let Some(token) = restore_token.filter(|token| !token.is_empty()) {
                options.insert("restore_token", Value::from(token));
            }
        }
        if let Some(mode) = cursor {
            options.insert("cursor_mode", Value::U32(mode));
        }
        screencast.call("SelectSources", &(session_object.clone(), options))
    })?;

    // -- Start: the dialog, or nothing at all if the token restored --
    let started = request(conn, &sender, "Start", deadline, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        // No parent window: the daemon has no surface to be modal over.
        screencast.call("Start", &(session_object.clone(), "", options))
    })?;
    let streams = started
        .get("streams")
        .map(|value| streams_from(value))
        .ok_or_else(|| PortalError::Protocol("Start returned no streams".to_string()))?;
    let restore_token = started.get("restore_token").and_then(|value| string_of(value));

    // -- OpenPipeWireRemote: no Request object, the fd comes straight back --
    let fd: zbus::zvariant::OwnedFd = screencast
        .call("OpenPipeWireRemote", &(session_object.clone(), HashMap::<&str, Value<'_>>::new()))
        .map_err(|err| classify("OpenPipeWireRemote", err))?;
    // Deserialising an `h` always yields the owned variant, so this
    // conversion moves the descriptor rather than duplicating it: one
    // owner, one close, on `Consent`'s drop.
    let pipewire_fd = OwnedFd::from(fd);

    Ok(Consent {
        streams,
        restore_token,
        version,
        persists,
        pipewire_fd,
        session: Session { conn: conn.clone(), path: session_path },
    })
}

/// Whether this portal understands `persist_mode` and `restore_token`.
///
/// The `version` property is the *negotiated* one - the frontend
/// reports the lower of its own and the desktop implementation's - so
/// it answers the only question that matters: will these two keys mean
/// anything to whoever handles the call.
fn persists(version: u32) -> bool {
    version >= PERSIST_MIN_VERSION
}

/// The best cursor mode this portal will actually accept. Setting a mode
/// the portal does not advertise *closes the session*, so an unadvertised
/// METADATA silently becomes HIDDEN, and a portal too old to have the
/// property at all gets no `cursor_mode` key (its default is Hidden).
fn cursor_mode(available: Option<u32>, want_metadata: bool) -> Option<u32> {
    let modes = available?;
    let wanted = if want_metadata { CURSOR_MODE_METADATA } else { CURSOR_MODE_HIDDEN };
    if modes & wanted != 0 {
        Some(wanted)
    } else if modes & CURSOR_MODE_HIDDEN != 0 {
        Some(CURSOR_MODE_HIDDEN)
    } else {
        None
    }
}

/// One portal method call, subscription first: predict the Request path,
/// register the match rule, *then* call, then wait for `Response`.
///
/// `call` receives the `handle_token` to put in its options and returns
/// the handle the portal replied with.
fn request(
    conn: &Connection,
    sender: &str,
    step: &'static str,
    deadline: Instant,
    call: impl FnOnce(&str) -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, PortalError> {
    let token = handle_token();
    let predicted = request_path(sender, &token);
    // Before the call, always: a restored session answers instantly.
    let watch = watch_response(conn, &predicted, step)?;

    let handle = call(&token).map_err(|err| classify(step, err))?;
    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // A portal older than xdg-desktop-portal 0.9, or one that
        // ignored `handle_token`. Listen where the handle actually is
        // and accept that a very fast reply may already be lost - the
        // deadline is what keeps that from hanging. The abandoned
        // iterator's thread ends when `open` closes the connection.
        drop(watch);
        watch_response(conn, handle.as_str(), step)?
    };

    watch.wait(step, deadline)
}

/// A `Request.Response` payload: the spec's `(ua{sv})`.
type Answer = Result<(u32, HashMap<String, OwnedValue>), PortalError>;

/// A registered subscription to one Request's `Response`, already being
/// pumped by its own thread.
struct ResponseWatch {
    rx: Receiver<Answer>,
}

/// Register the match rule for `Response` at `path` and start pumping
/// it. Returns once the bus has the rule, which is the whole point:
/// the caller may only issue its method after this.
fn watch_response(
    conn: &Connection,
    path: &str,
    step: &'static str,
) -> Result<ResponseWatch, PortalError> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(PORTAL_BUS)
        .and_then(|builder| builder.path(path.to_string()))
        .and_then(|builder| builder.interface(REQUEST_INTERFACE))
        .and_then(|builder| builder.member("Response"))
        .map_err(|err| PortalError::Protocol(format!("{step}: bad match rule for {path}: {err}")))?
        .build();
    // One Response per Request, so the queue only needs to outlive the
    // gap between registering and reading.
    let iterator = MessageIterator::for_match_rule(rule, conn, Some(2))
        .map_err(|err| classify(step, err))?;

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("chibipop-portal-req".to_string())
        .spawn(move || {
            // Exactly one Response per Request: take the first message
            // and let the thread end.
            let answer = match iterator.into_iter().next() {
                Some(Ok(message)) => message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map_err(|err| {
                        PortalError::Protocol(format!("{step}: malformed Response: {err}"))
                    }),
                Some(Err(err)) => {
                    Err(PortalError::Protocol(format!("{step}: bus error waiting: {err}")))
                }
                // The iterator ended: the connection closed under us,
                // which is how an abandoned wait is reaped.
                None => Err(PortalError::Protocol(format!(
                    "{step}: the session bus closed before the portal answered"
                ))),
            };
            let _ = tx.send(answer);
        })
        .map_err(|err| PortalError::Protocol(format!("{step}: no thread for the wait: {err}")))?;

    Ok(ResponseWatch { rx })
}

impl ResponseWatch {
    /// Block until the portal answers or `deadline` passes, mapping the
    /// spec's three response codes onto [`PortalError`].
    fn wait(
        self,
        step: &'static str,
        deadline: Instant,
    ) -> Result<HashMap<String, OwnedValue>, PortalError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(remaining) {
            Ok(Ok((RESPONSE_SUCCESS, results))) => Ok(results),
            Ok(Ok((RESPONSE_CANCELLED, _))) => Err(PortalError::Denied),
            Ok(Ok((RESPONSE_ENDED, _))) => Err(PortalError::Ended(format!(
                "the portal closed {step} itself; a stale restore token looks like this too"
            ))),
            Ok(Ok((code, _))) => Err(PortalError::Protocol(format!(
                "{step}: response code {code} is not one the spec defines"
            ))),
            Ok(Err(err)) => Err(err),
            Err(RecvTimeoutError::Timeout) => Err(PortalError::TimedOut(step.to_string())),
            Err(RecvTimeoutError::Disconnected) => Err(PortalError::Protocol(format!(
                "{step}: the waiting thread stopped without answering"
            ))),
        }
    }
}

/// The ScreenCast proxy on the portal's single object.
fn screencast_proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(
        conn.clone(),
        PORTAL_BUS.to_string(),
        PORTAL_PATH.to_string(),
        SCREENCAST_INTERFACE.to_string(),
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
/// this process by the counter and unguessable enough by the clock. No
/// `rand` dependency for this - the token is a collision guard against
/// other libraries on the same connection, not a secret.
fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    format!("chibipop_{}_{seq}_{nanos}", std::process::id())
}

/// Every stream in a `Start` result's `streams` (`a(ua{sv})`). An entry
/// that is not shaped like a stream is dropped, not fatal: the rest of
/// the monitors are still usable.
fn streams_from(value: &Value<'_>) -> Vec<StreamInfo> {
    let Value::Array(items) = peel(value) else {
        return Vec::new();
    };
    let mut streams = Vec::with_capacity(items.len());
    for item in items.iter() {
        let Value::Structure(fields) = peel(item) else {
            continue;
        };
        let fields = fields.fields();
        let (Some(Value::U32(node_id)), Some(Value::Dict(dict))) =
            (fields.first().map(peel), fields.get(1).map(peel))
        else {
            continue;
        };
        let mut props = HashMap::new();
        for (key, value) in dict.iter() {
            let Some(key) = string_of(key) else {
                continue;
            };
            if let Ok(owned) = OwnedValue::try_from(value) {
                props.insert(key, owned);
            }
        }
        streams.push(stream_info_from(*node_id, &props));
    }
    streams
}

/// One stream's optional properties, defensively. Every property here
/// arrived in a later interface version than the last, so a missing key
/// is ordinary and a wrongly-typed one is a portal bug we survive: both
/// answer `None` rather than sink the whole handshake.
fn stream_info_from(node_id: u32, props: &HashMap<String, OwnedValue>) -> StreamInfo {
    StreamInfo {
        node_id,
        position: props.get("position").and_then(|value| pair_of(value)),
        size: props.get("size").and_then(|value| pair_of(value)),
        source_type: props.get("source_type").and_then(|value| u32_of(value)),
    }
}

/// A variant may itself hold a variant; look through those wrappers so a
/// property is read by its type, not by how it was boxed.
fn peel<'a, 'v>(value: &'a Value<'v>) -> &'a Value<'v> {
    match value {
        Value::Value(inner) => peel(inner),
        other => other,
    }
}

/// An `(ii)` property: exactly two `i32`s, or nothing.
fn pair_of(value: &Value<'_>) -> Option<(i32, i32)> {
    let Value::Structure(structure) = peel(value) else {
        return None;
    };
    let fields = structure.fields();
    if fields.len() != 2 {
        return None;
    }
    match (fields.first().map(peel), fields.get(1).map(peel)) {
        (Some(Value::I32(x)), Some(Value::I32(y))) => Some((*x, *y)),
        _ => None,
    }
}

/// A `u` property, or nothing.
fn u32_of(value: &Value<'_>) -> Option<u32> {
    match peel(value) {
        Value::U32(n) => Some(*n),
        _ => None,
    }
}

/// An `s` property. Object paths are accepted too: `session_handle` is
/// specified as `s` but reads as a path, and portals have shipped both.
fn string_of(value: &Value<'_>) -> Option<String> {
    match peel(value) {
        Value::Str(text) => Some(text.as_str().to_string()),
        Value::ObjectPath(path) => Some(path.as_str().to_string()),
        _ => None,
    }
}

/// A zbus failure, sorted into "this rung is not here" versus "this
/// rung misbehaved". The distinction is what the ladder needs: absence
/// is skipped quietly, misbehaviour is reported.
fn classify(step: &str, err: zbus::Error) -> PortalError {
    match &err {
        zbus::Error::MethodError(name, _, _) => {
            let name = name.as_str();
            if matches!(
                name,
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    | "org.freedesktop.DBus.Error.UnknownInterface"
                    | "org.freedesktop.DBus.Error.UnknownObject"
                    | "org.freedesktop.DBus.Error.UnknownProperty"
                    | "org.freedesktop.DBus.Error.UnknownMethod"
            ) {
                PortalError::Absent(format!("{step}: {name}"))
            } else {
                PortalError::Protocol(format!("{step}: {err}"))
            }
        }
        zbus::Error::Address(_)
        | zbus::Error::Connection(_, _)
        | zbus::Error::InputOutput(_)
        | zbus::Error::Handshake(_) => PortalError::Absent(format!("{step}: {err}")),
        _ => PortalError::Protocol(format!("{step}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A property dict the way `Start` sends one, without a bus.
    fn props(entries: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(key, value)| {
                let owned = OwnedValue::try_from(value).expect("a test value is ownable");
                (key.to_string(), owned)
            })
            .collect()
    }

    // -- the predicted Request path (the race avoidance) --

    /// The mangling the Request documentation specifies, and the only
    /// reason a subscription can precede its call.
    #[test]
    fn a_unique_bus_name_becomes_a_path_element() {
        assert_eq!(mangle_sender(":1.234"), "1_234");
        assert_eq!(mangle_sender(":1.2.345"), "1_2_345");
        assert_eq!(mangle_sender("1.42"), "1_42");
    }

    #[test]
    fn a_predicted_request_path_sits_under_the_portal_object() {
        assert_eq!(
            request_path("1_234", "chibipop_9_0_1"),
            "/org/freedesktop/portal/desktop/request/1_234/chibipop_9_0_1"
        );
    }

    /// A token that is not a valid object-path element makes the portal
    /// reject the call outright, so the alphabet is part of the contract.
    #[test]
    fn a_handle_token_is_a_valid_path_element() {
        let token = handle_token();
        assert!(!token.is_empty());
        assert!(
            token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "token {token:?} must match [A-Za-z0-9_]+"
        );
    }

    #[test]
    fn handle_tokens_differ_across_calls() {
        let first = handle_token();
        let second = handle_token();
        assert_ne!(first, second);
    }

    // -- what the tray and the log get to say --

    /// ADR-0006: a status row names the way back, on one line.
    #[test]
    fn every_failure_names_a_way_back() {
        let failures = [
            PortalError::Absent("no owner".to_string()),
            PortalError::Denied,
            PortalError::Ended("token refused".to_string()),
            PortalError::TimedOut("Start".to_string()),
            PortalError::Protocol("code 7".to_string()),
        ];
        for failure in failures {
            let detail = failure.detail();
            assert!(!detail.is_empty(), "{failure:?}");
            assert!(!detail.contains('\n'), "{detail}");
            assert!(detail.contains("retry"), "{detail}");
        }
    }

    /// The timeout must name the dialog nobody answered, or the log
    /// cannot say which step went quiet.
    #[test]
    fn a_timeout_names_the_step_that_went_unanswered() {
        assert!(PortalError::TimedOut("SelectSources".to_string()).detail().contains("SelectSources"));
    }

    /// The token is only worthless when consent itself was refused.
    #[test]
    fn only_a_refused_session_needs_fresh_consent() {
        assert!(PortalError::Denied.retry_needs_fresh_consent());
        assert!(PortalError::Ended("stale token".to_string()).retry_needs_fresh_consent());
        assert!(!PortalError::TimedOut("Start".to_string()).retry_needs_fresh_consent());
        assert!(!PortalError::Absent("no portal".to_string()).retry_needs_fresh_consent());
        assert!(!PortalError::Protocol("nonsense".to_string()).retry_needs_fresh_consent());
    }

    // -- stream properties --

    #[test]
    fn stream_properties_parse_into_logical_geometry() {
        let info = stream_info_from(
            42,
            &props(vec![
                ("position", Value::from((100i32, -50i32))),
                ("size", Value::from((2560i32, 1440i32))),
                ("source_type", Value::U32(SOURCE_MONITOR)),
            ]),
        );
        assert_eq!(
            info,
            StreamInfo {
                node_id: 42,
                position: Some((100, -50)),
                size: Some((2560, 1440)),
                source_type: Some(SOURCE_MONITOR),
            }
        );
    }

    /// Every one of these properties arrived in a later interface
    /// version than the last, so absence is ordinary.
    #[test]
    fn a_stream_without_properties_is_still_a_stream() {
        let info = stream_info_from(7, &props(vec![]));
        assert_eq!(
            info,
            StreamInfo { node_id: 7, position: None, size: None, source_type: None }
        );
    }

    /// A portal sending the wrong type must cost that one property, not
    /// the whole handshake.
    #[test]
    fn wrongly_typed_stream_properties_are_ignored() {
        let info = stream_info_from(
            3,
            &props(vec![
                ("position", Value::from("nowhere")),
                ("size", Value::U32(1440)),
                ("source_type", Value::from("monitor")),
            ]),
        );
        assert_eq!(
            info,
            StreamInfo { node_id: 3, position: None, size: None, source_type: None }
        );
    }

    /// An `(iii)` where `(ii)` was promised is still the wrong type.
    #[test]
    fn a_position_of_the_wrong_arity_is_ignored() {
        let info = stream_info_from(1, &props(vec![("position", Value::from((1i32, 2i32, 3i32)))]));
        assert_eq!(info.position, None);
    }

    /// A variant nested inside a variant reads as the value it holds.
    #[test]
    fn a_doubly_boxed_property_still_reads() {
        let boxed = Value::Value(Box::new(Value::U32(SOURCE_MONITOR)));
        let info = stream_info_from(1, &props(vec![("source_type", boxed)]));
        assert_eq!(info.source_type, Some(SOURCE_MONITOR));
    }

    // -- the persist gate (ADR-0002's silent relaunch) --

    /// `persist_mode`/`restore_token` are ScreenCast v4 keys. Sending
    /// them to an older portal is not an error, but the daemon has to
    /// know it happened so it can say *why* no token came back:
    /// xdg-desktop-portal-hyprland reports v3 today, so this is the
    /// live case on a wlr desk, not a hypothetical.
    #[test]
    fn only_a_v4_portal_is_sent_the_persist_keys() {
        assert_eq!(4, PERSIST_MIN_VERSION);
        assert!(!persists(1));
        assert!(!persists(3), "xdg-desktop-portal-hyprland's version today");
        assert!(persists(PERSIST_MIN_VERSION));
        assert!(persists(5), "a newer portal keeps the keys");
    }

    // -- the cursor rung's capability check (ADR-0003) --

    /// EMBEDDED (2) has no constant of its own: this backend never
    /// asks for it, and a portal advertising it changes nothing.
    const EMBEDDED: u32 = 2;

    #[test]
    fn a_portal_offering_metadata_cursors_gets_asked_for_them() {
        let modes = CURSOR_MODE_HIDDEN | EMBEDDED | CURSOR_MODE_METADATA;
        assert_eq!(cursor_mode(Some(modes), true), Some(CURSOR_MODE_METADATA));
        assert_eq!(cursor_mode(Some(modes), false), Some(CURSOR_MODE_HIDDEN));
    }

    /// Asking for an unadvertised mode closes the session, so it must
    /// degrade instead.
    #[test]
    fn a_portal_without_metadata_cursors_degrades_to_hidden() {
        let modes = CURSOR_MODE_HIDDEN | EMBEDDED;
        assert_eq!(cursor_mode(Some(modes), true), Some(CURSOR_MODE_HIDDEN));
    }

    /// A portal too old for `AvailableCursorModes` predates
    /// `cursor_mode` itself: send no key and take its Hidden default.
    #[test]
    fn a_portal_without_the_property_is_sent_no_cursor_mode() {
        assert_eq!(cursor_mode(None, true), None);
        assert_eq!(cursor_mode(Some(0), true), None);
    }
}
