//! This module implements the `org.freedesktop.portal.ScreenCast` handshake
//! for the fallback capture rung (ARCHITECTURE.md#capture-and-masking).
//! The handshake starts the consent dialog and returns a PipeWire remote file descriptor.
//!
//! **Why consent starts early.** The portal is the only component that asks
//! for capture permission. The daemon asks once at startup for all monitors
//! (`multiple = true`). This avoids a dialog when the first hover occurs.
//! `Start` returns a restore token. [`super::token`] stores this token, and
//! later launches use it without a dialog.
//!
//! **Why this module uses `zbus::blocking` instead of `ashpd`.** `ashpd` needs
//! an asynchronous runtime. The design rejects a second asynchronous runtime
//! because the calloop pump is synchronous. The tree already uses zbus because
//! ksni depends on the same version. The `zbus::blocking` API uses the
//! `async-io` executor that ksni starts. The caller chooses the thread for
//! these operations. The daemon uses a portal thread, so its loop does not
//! wait for a dialog.
//!
//! **How this module prevents the Request race.** Each portal method has two
//! replies. The method reply contains an `o` Request handle. A later `Response`
//! (`(ua{sv})`) signal on that object contains the result. Code that subscribes
//! after the method reply can miss a fast `Response`. A restored session can
//! send this signal without a dialog. xdg-desktop-portal gives each handle
//! this path:
//! `/org/freedesktop/portal/desktop/request/<SENDER>/<handle_token>`.
//! The portal derives `<SENDER>` from the unique D-Bus name. It removes the
//! first `:` and replaces each `.` with `_`. Each call adds its match rule at
//! the predicted path before it sends the method. The call checks the returned
//! handle against that path. If an older portal returns another path, the call
//! subscribes to the returned path.
//!
//! **Why this module uses a waiter thread.** The zbus signal iterator has no
//! wait limit. One deadline covers the full handshake, so an unanswered dialog
//! cannot block startup. Each wait gives its iterator to a new thread. The
//! thread sends the first `Response` through an `mpsc` channel. The caller uses
//! `recv_timeout`. If the handshake stops, the caller closes the shared
//! connection. This stops the iterators and their threads. It also discards the
//! incomplete session.
//!
//! Developers compared the D-Bus signatures with
//! `/usr/share/dbus-1/interfaces/org.freedesktop.portal.ScreenCast.xml`
//! and `org.freedesktop.portal.Request.xml` on this machine. The ScreenCast
//! interface file has version 6.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

/// The well-known D-Bus name for the portal.
pub const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
/// The object path that contains all portal interfaces.
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The ScreenCast interface that this module uses.
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";

/// The monitor source type for `SelectSources`.
pub const SOURCE_MONITOR: u32 = 1;
/// Cursor modes for `AvailableCursorModes` and `SelectSources.cursor_mode`.
/// This module does not use SPA mode EMBEDDED (2). EMBEDDED adds the cursor
/// to OCR pixels, but the cursor rung needs cursor coordinates.
pub const CURSOR_MODE_HIDDEN: u32 = 1;
/// The portal sends the cursor as PipeWire metadata next to the pixels. This
/// mode supplies cursor rung 2.
pub const CURSOR_MODE_METADATA: u32 = 4;
/// The `persist_mode` value 2 keeps consent until the user revokes it.
pub const PERSIST_UNTIL_REVOKED: u32 = 2;
/// `persist_mode` and `restore_token` need ScreenCast version 4.
///
/// xdg-desktop-portal-hyprland reports version 3. It cannot store a grant,
/// so each launch shows the dialog. The portal ignores unknown options, but
/// these keys would hide why the portal returned no token. The code must
/// distinguish unsupported persistence from a refused grant.
pub const PERSIST_MIN_VERSION: u32 = 4;

/// The interface that sends the deferred response for each portal method.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
/// The interface that closes a portal session.
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `Response` code 0 means that the portal completed the request.
const RESPONSE_SUCCESS: u32 = 0;
/// `Response` code 1 means that the user canceled the request.
const RESPONSE_CANCELLED: u32 = 1;
/// `Response` code 2 means that the portal ended the request. A stale
/// restore token also produces this code.
const RESPONSE_ENDED: u32 = 2;

/// A monitor stream that `Start` returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// The PipeWire node for this stream.
    pub node_id: u32,
    /// The `position` property in logical layout coordinates, if present.
    pub position: Option<(i32, i32)>,
    /// The `size` property in logical units, if present.
    pub size: Option<(i32, i32)>,
    /// The `source_type` property, if present.
    pub source_type: Option<u32>,
}

/// The reason that the portal rung cannot serve requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalError {
    /// The session bus has no portal, or the portal has no ScreenCast interface.
    Absent(String),
    /// The user refused the request with Request response code 1.
    Denied,
    /// The portal ended the request with response code 2. It can also reject a
    /// stale restore token with this code.
    Ended(String),
    /// The handshake did not finish before the deadline.
    TimedOut(String),
    /// The portal returned a result that the specification does not define.
    Protocol(String),
}

impl PortalError {
    /// Returns text for a tray or settings row. The text names the fault and
    /// gives the user a retry action.
    pub fn detail(&self) -> String {
        match self {
            PortalError::Absent(what) => format!(
                "no screen-capture portal on the session bus ({what}) - install xdg-desktop-portal \
                 and its compositor backend, then retry with `chibipop ctl reload`"
            ),
            // Tray rows show status and accept no input. The `reload` verb supplies the
            // retry action. The settings window sends this verb when the user selects
            // Apply. The text names both paths because a GNOME session can have no tray.
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

    /// Returns whether the caller must remove the stored restore token before
    /// a retry. A denial or a rejected token needs fresh consent.
    pub fn retry_needs_fresh_consent(&self) -> bool {
        match self {
            // A restore token cannot change the user's refusal.
            PortalError::Denied => true,
            // Code 2 can mean that the token is stale or revoked.
            PortalError::Ended(_) => true,
            // The other errors do not invalidate the grant.
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

/// A live ScreenCast session and the results from `Start`.
pub struct Consent {
    /// One entry for each shared monitor, in portal order.
    pub streams: Vec<StreamInfo>,
    /// The restore token for the next launch. `None` means that the portal
    /// did not return a token. See [`Consent::persists`] for support details.
    pub restore_token: Option<String>,
    /// The ScreenCast interface version that the portal reports.
    pub version: u32,
    /// Whether this ScreenCast version can store a grant. If true, a
    /// `restore_token` value of `None` means that the portal chose not to return one.
    pub persists: bool,
    /// The PipeWire remote file descriptor from `OpenPipeWireRemote`.
    pub pipewire_fd: OwnedFd,
    /// Keep this value while the caller needs frames. `Drop` closes the
    /// session and revokes the grant.
    pub session: Session,
}

/// The portal session object. Its `Drop` implementation calls `Close`.
pub struct Session {
    /// The D-Bus connection that created the session. `Close` must use this
    /// same connection.
    conn: Connection,
    /// The session object path from the `CreateSession` results.
    path: String,
}

impl Session {
    /// Returns the session object path that the portal reported.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Closes the session now. `Drop` also calls this method and ignores errors.
    pub fn close(&self) {
        let Ok(proxy) = Proxy::new(&self.conn, PORTAL_BUS, self.path.as_str(), SESSION_INTERFACE)
        else {
            return;
        };
        // The portal can already have closed the session. A lost bus or a second
        // `close` needs no recovery. The daemon does not need a log for these states.
        let _: zbus::Result<()> = proxy.call("Close", &());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// Returns whether `org.freedesktop.portal.ScreenCast` responds on the
/// session bus. An absent bus or portal returns `false`.
pub fn probe() -> bool {
    let Ok(conn) = Connection::session() else {
        return false;
    };
    let Ok(proxy) = screencast_proxy(&conn) else {
        return false;
    };
    // The `version` property confirms that the interface exists. A D-Bus name
    // without ScreenCast fails this check.
    proxy.get_property::<u32>("version").is_ok()
}

/// Returns the portal cursor modes. Returns `None` when the property cannot
/// be read.
pub fn available_cursor_modes() -> Option<u32> {
    let conn = Connection::session().ok()?;
    screencast_proxy(&conn).ok()?.get_property::<u32>("AvailableCursorModes").ok()
}

/// Starts the complete consent flow at startup. It calls `CreateSession`,
/// `SelectSources`, `Start`, and `OpenPipeWireRemote` in this order.
/// `SelectSources` requests all monitors in one dialog. The call blocks
/// the current thread for at most `timeout` in total. The previous
/// `restore_token` can prevent a later dialog.
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
            // The code closes the connection after a handshake error. This removes the
            // incomplete session and stops each `Response` waiter thread.
            let _ = conn.close();
            Err(err)
        }
    }
}

/// Calls the four portal methods in the only valid order.
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
    // This check confirms that the interface exists before a dialog uses the
    // startup deadline. The capture ladder can skip an absent portal.
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
    // The specification declares `session_handle` as `s`, but the value is a
    // path. The code parses it as a path before it sends the value back.
    let session_object = ObjectPath::try_from(session_path.clone()).map_err(|err| {
        PortalError::Protocol(format!("session_handle {session_path:?} is not a path: {err}"))
    })?;
    // Every failure after `CreateSession` needs `Close`. `open` closes the
    // connection to provide this cleanup.

    // -- SelectSources: one dialog, every monitor --
    request(conn, &sender, "SelectSources", deadline, |token| {
        let options = select_sources_options(token, cursor, persists, restore_token);
        screencast.call("SelectSources", &(session_object.clone(), options))
    })?;

    // -- Start: show a dialog unless the restore token works --
    let started = request(conn, &sender, "Start", deadline, |token| {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token));
        // The daemon has no parent surface for a modal dialog.
        screencast.call("Start", &(session_object.clone(), "", options))
    })?;
    let streams = started
        .get("streams")
        .map(|value| streams_from(value))
        .ok_or_else(|| PortalError::Protocol("Start returned no streams".to_string()))?;
    let restore_token = started.get("restore_token").and_then(|value| string_of(value));

    // -- OpenPipeWireRemote returns its file descriptor directly --
    let fd: zbus::zvariant::OwnedFd = screencast
        .call("OpenPipeWireRemote", &(session_object.clone(), HashMap::<&str, Value<'_>>::new()))
        .map_err(|err| classify("OpenPipeWireRemote", err))?;
    // zbus deserializes an `h` value as an owned variant. This conversion moves
    // the descriptor without a copy. `Consent` owns and closes it.
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

/// Returns whether this portal supports `persist_mode` and `restore_token`.
///
/// The portal front end reports the lower version from itself and the desktop
/// implementation. Therefore, `version` shows whether the handler supports
/// both keys.
fn persists(version: u32) -> bool {
    version >= PERSIST_MIN_VERSION
}

/// Selects a cursor mode that the portal accepts. An unsupported mode closes
/// the session. If METADATA is unavailable, this function selects HIDDEN. If
/// the property is absent, it returns `None` and the portal uses Hidden.
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

/// Builds every `SelectSources` option in one function. Tests can inspect
/// the exact options.
///
/// The cursor mode and restore token use one option map. The ScreenCast
/// version 6 specification permits one source selection per session. It
/// defines `restore_token` as a `SelectSources` option. Each launch selects
/// sources again after a restore. A token restores shared monitors and
/// permission, but it does not restore a cursor mode. Therefore, a restored
/// grant cannot keep an earlier cursor mode.
fn select_sources_options<'a>(
    handle_token: &'a str,
    cursor: Option<u32>,
    persists: bool,
    restore_token: Option<&'a str>,
) -> HashMap<&'static str, Value<'a>> {
    let mut options: HashMap<&'static str, Value<'a>> = HashMap::new();
    options.insert("handle_token", Value::from(handle_token));
    options.insert("types", Value::U32(SOURCE_MONITOR));
    options.insert("multiple", Value::Bool(true));
    // Send version 4 keys only to a portal that supports them.
    if persists {
        options.insert("persist_mode", Value::U32(PERSIST_UNTIL_REVOKED));
        if let Some(token) = restore_token.filter(|token| !token.is_empty()) {
            options.insert("restore_token", Value::from(token));
        }
    }
    if let Some(mode) = cursor {
        options.insert("cursor_mode", Value::U32(mode));
    }
    options
}

/// Calls one portal method after it registers the `Response` subscription.
/// The code predicts the Request path and registers the match rule before
/// the method call.
///
/// `call` receives the `handle_token` and returns the portal Request handle.
fn request(
    conn: &Connection,
    sender: &str,
    step: &'static str,
    deadline: Instant,
    call: impl FnOnce(&str) -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, PortalError> {
    let token = handle_token();
    let predicted = request_path(sender, &token);
    // Register the watch before the call. A restored session can answer instantly.
    let watch = watch_response(conn, &predicted, step)?;

    let handle = call(&token).map_err(|err| classify(step, err))?;
    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // A portal older than xdg-desktop-portal 0.9 can ignore `handle_token`.
        // Listen at the returned handle path in that case. A fast reply can already
        // be lost. The deadline prevents an indefinite wait. The abandoned iterator
        // thread ends when `open` closes the connection.
        drop(watch);
        watch_response(conn, handle.as_str(), step)?
    };

    watch.wait(step, deadline)
}

/// A `Request.Response` payload: the spec's `(ua{sv})`.
type Answer = Result<(u32, HashMap<String, OwnedValue>), PortalError>;

/// A registered subscription to one Request's `Response`. A separate thread
/// reads its messages.
struct ResponseWatch {
    rx: Receiver<Answer>,
}

/// Registers the match rule for `Response` at `path` and starts the reader
/// thread. The function returns after the bus adds the rule. The caller can
/// issue the method only after this point.
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
    // One Response exists for each Request. The queue only needs to outlive the
    // interval between registration and the first read.
    let iterator = MessageIterator::for_match_rule(rule, conn, Some(2))
        .map_err(|err| classify(step, err))?;

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("chibipop-portal-req".to_string())
        .spawn(move || {
            // Each Request has one Response. The thread reads the first message and then
            // stops.
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
                // The iterator ended because the connection closed. This ends an abandoned
                // wait.
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
    /// Waits until the portal answers or `deadline` passes. It maps the
    /// specification's three response codes to [`PortalError`].
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

/// Converts the unique bus name to an object-path element. It drops the
/// first `:` and replaces every `.` with `_`, as the Request documentation
/// specifies.
fn mangle_sender(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

/// Returns the path where the portal places the Request object for `token`.
fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Creates a fresh `handle_token`. The token is a valid object-path element.
/// The counter and clock make it unique within this process. The code needs
/// no `rand` dependency. The token prevents collisions with other libraries
/// on the same connection. It is not a secret.
fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    format!("chibipop_{}_{seq}_{nanos}", std::process::id())
}

/// Reads every stream in a `Start` result's `streams` (`a(ua{sv})`). It drops
/// entries with another shape, but it keeps all valid monitor entries.
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

/// Reads optional properties for one stream. Each property arrived in a later
/// interface version, so an absent key is normal. A wrong type means a portal
/// bug, but the code returns `None` for that property and keeps the handshake.
fn stream_info_from(node_id: u32, props: &HashMap<String, OwnedValue>) -> StreamInfo {
    StreamInfo {
        node_id,
        position: props.get("position").and_then(|value| pair_of(value)),
        size: props.get("size").and_then(|value| pair_of(value)),
        source_type: props.get("source_type").and_then(|value| u32_of(value)),
    }
}

/// Looks through nested variants. This lets the code read a property by type
/// instead of by its wrapper depth.
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

/// Reads an `s` property. It also accepts an object path because the
/// specification defines `session_handle` as `s`, but portals can send a path.
fn string_of(value: &Value<'_>) -> Option<String> {
    match peel(value) {
        Value::Str(text) => Some(text.as_str().to_string()),
        Value::ObjectPath(path) => Some(path.as_str().to_string()),
        _ => None,
    }
}

/// Classifies a zbus failure as an absent rung or a faulty rung. The capture
/// ladder skips absence and reports a fault.
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

    /// Creates a property dict in the form that `Start` sends, without a bus.
    fn props(entries: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(key, value)| {
                let owned = OwnedValue::try_from(value).expect("a test value is ownable");
                (key.to_string(), owned)
            })
            .collect()
    }

    // -- predicted Request path --

    /// Tests the name conversion that the Request documentation specifies. The
    /// result lets the code register a subscription before the call.
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

    /// The portal rejects an invalid object-path element before the call. The
    /// token alphabet is therefore part of the contract.
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

    // -- tray and log text --

    /// A status row gives one retry action on one line.
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

    /// The timeout must name the unanswered dialog. Otherwise, the log cannot
    /// identify the step that received no response.
    #[test]
    fn a_timeout_names_the_step_that_went_unanswered() {
        assert!(PortalError::TimedOut("SelectSources".to_string()).detail().contains("SelectSources"));
    }

    /// Fresh consent is needed when the portal refuses consent or rejects a token.
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

    /// These properties came with later interface versions. An absent property
    /// is therefore normal.
    #[test]
    fn a_stream_without_properties_is_still_a_stream() {
        let info = stream_info_from(7, &props(vec![]));
        assert_eq!(
            info,
            StreamInfo { node_id: 7, position: None, size: None, source_type: None }
        );
    }

    /// A wrong property type affects that property only. It must not fail the
    /// whole handshake.
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

    /// The property requires `(ii)`. An `(iii)` value has the wrong type.
    #[test]
    fn a_position_of_the_wrong_arity_is_ignored() {
        let info = stream_info_from(1, &props(vec![("position", Value::from((1i32, 2i32, 3i32)))]));
        assert_eq!(info.position, None);
    }

    /// A variant can contain another variant. The code must read the inner value.
    #[test]
    fn a_doubly_boxed_property_still_reads() {
        let boxed = Value::Value(Box::new(Value::U32(SOURCE_MONITOR)));
        let info = stream_info_from(1, &props(vec![("source_type", boxed)]));
        assert_eq!(info.source_type, Some(SOURCE_MONITOR));
    }

    // -- persist gate --

    /// `persist_mode` and `restore_token` are ScreenCast v4 keys. The code does
    /// not send them to an older portal. The daemon must know this case so it
    /// can explain why the portal returned no token. xdg-desktop-portal-hyprland
    /// reports v3, so this case occurs on a wlr desk.
    #[test]
    fn only_a_v4_portal_is_sent_the_persist_keys() {
        assert_eq!(4, PERSIST_MIN_VERSION);
        assert!(!persists(1));
        assert!(!persists(3), "xdg-desktop-portal-hyprland's version today");
        assert!(persists(PERSIST_MIN_VERSION));
        assert!(persists(5), "a newer portal keeps the keys");
    }

    // -- the cursor rung's capability check --

    /// EMBEDDED (2) has no constant here. This backend never requests it, and a
    /// portal that advertises it does not change the result.
    const EMBEDDED: u32 = 2;

    #[test]
    fn a_portal_offering_metadata_cursors_gets_asked_for_them() {
        let modes = CURSOR_MODE_HIDDEN | EMBEDDED | CURSOR_MODE_METADATA;
        assert_eq!(cursor_mode(Some(modes), true), Some(CURSOR_MODE_METADATA));
        assert_eq!(cursor_mode(Some(modes), false), Some(CURSOR_MODE_HIDDEN));
    }

    /// The portal closes the session for an unadvertised mode. The code must
    /// select a supported mode instead.
    #[test]
    fn a_portal_without_metadata_cursors_degrades_to_hidden() {
        let modes = CURSOR_MODE_HIDDEN | EMBEDDED;
        assert_eq!(cursor_mode(Some(modes), true), Some(CURSOR_MODE_HIDDEN));
    }

    /// A portal without `AvailableCursorModes` also predates `cursor_mode`. The
    /// code sends no key, so the portal uses its Hidden default.
    #[test]
    fn a_portal_without_the_property_is_sent_no_cursor_mode() {
        assert_eq!(cursor_mode(None, true), None);
        assert_eq!(cursor_mode(Some(0), true), None);
    }

    // -- embedded cursor exclusion --

    /// This test covers every combination of the four defined modes. The result
    /// never requests EMBEDDED. An EMBEDDED-only portal receives no key and keeps
    /// its Hidden default. The portal does not paint the cursor into OCR pixels.
    #[test]
    fn no_advertised_mode_set_ever_asks_for_an_embedded_cursor() {
        for modes in 0u32..16 {
            for want_metadata in [false, true] {
                let got = cursor_mode(Some(modes), want_metadata);
                assert_ne!(Some(EMBEDDED), got, "modes {modes:#b} metadata {want_metadata}");
            }
        }
        assert_eq!(None, cursor_mode(Some(EMBEDDED), false), "embedded-only offers nothing");
    }

    /// A restored session can use a cursor mode from an earlier negotiation if
    /// the code sends no new mode. `restore_token` is a `SelectSources` option,
    /// so it uses the same dict as `cursor_mode`. A restored grant cannot carry
    /// an earlier cursor mode.
    #[test]
    fn a_restored_session_selects_sources_with_the_cursor_mode_too() {
        let options =
            select_sources_options("tok", Some(CURSOR_MODE_HIDDEN), true, Some("stored-token"));
        assert_eq!(Some(&Value::from("stored-token")), options.get("restore_token"));
        assert_eq!(Some(&Value::U32(CURSOR_MODE_HIDDEN)), options.get("cursor_mode"));
        assert_eq!(Some(&Value::U32(PERSIST_UNTIL_REVOKED)), options.get("persist_mode"));
        assert_eq!(Some(&Value::Bool(true)), options.get("multiple"));
    }

    /// A first launch differs only by its token. A v3 portal receives neither
    /// persist key, but both launch types still receive the cursor mode.
    #[test]
    fn a_fresh_or_unpersistable_session_still_sends_the_cursor_mode() {
        let fresh = select_sources_options("tok", Some(CURSOR_MODE_METADATA), true, None);
        assert_eq!(None, fresh.get("restore_token"), "nothing to restore yet");
        assert_eq!(Some(&Value::U32(CURSOR_MODE_METADATA)), fresh.get("cursor_mode"));

        let old = select_sources_options("tok", Some(CURSOR_MODE_HIDDEN), false, Some("ignored"));
        assert_eq!(None, old.get("persist_mode"), "v3 gets no persist keys");
        assert_eq!(None, old.get("restore_token"), "nor a token it cannot use");
        assert_eq!(Some(&Value::U32(CURSOR_MODE_HIDDEN)), old.get("cursor_mode"));
    }

    /// An empty stored token is a first launch, not a token.
    #[test]
    fn a_blank_stored_token_is_not_sent() {
        let options = select_sources_options("tok", Some(CURSOR_MODE_HIDDEN), true, Some(""));
        assert_eq!(None, options.get("restore_token"));
    }
}
