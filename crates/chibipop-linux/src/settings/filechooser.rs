//! `org.freedesktop.portal.FileChooser`: the Add row's Browse button.
//!
//! **Why the portal.** The settings window is a Wayland client with no
//! toolkit file dialog of its own (it stays iced-native), and
//! `OpenFile` is the one dialog every supported desktop already
//! implements — KDE draws Plasma's, GNOME draws GTK's — so a Browse
//! button costs a D-Bus call instead of a second widget stack. It is
//! also the only picker that works from a Flatpak, which is where the
//! portal's document store hands back real paths under `/run/user`.
//!
//! **Why blocking zbus on a borrowed thread.** Same bargain the capture
//! and shortcuts portals strike ([`crate::shortcuts::portal`]): zbus is
//! already in the tree with a blocking API, so nothing here needs an
//! async runtime. A dialog waits on a human, so the call cannot run on
//! iced's executor — [`pick`] is given its own thread by the caller and
//! answers once, on a channel.
//!
//! **The Request race** is avoided exactly as the other two do it: the
//! Request object path is predictable (`…/request/<SENDER>/<token>`), so
//! the match rule goes on *before* the call. A portal that remembers a
//! previous answer can respond faster than a subscription made after.
//!
//! Signatures were verified against
//! `/usr/share/dbus-1/interfaces/org.freedesktop.portal.FileChooser.xml`
//! (interface version 4) rather than from memory: `OpenFile` takes
//! `(parent_window: s, title: s, options: a{sv})` and answers `o`, and
//! its `Response` carries `uris` as an array of strings.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MessageType;
use zbus::zvariant::{Array, OwnedValue, Signature, Structure, Value};
use zbus::MatchRule;

/// The portal's well-known bus name.
const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
/// The portal's single object path; every portal interface lives here.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The interface this module speaks.
const FILECHOOSER_INTERFACE: &str = "org.freedesktop.portal.FileChooser";
/// Where a portal method's deferred answer arrives.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// `Response` code 0: carried out. 1: the user dismissed the dialog.
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// A file dialog waits on a human reading their disk, so the budget is a
/// human's. Nothing blocks on it: the window keeps rendering and only
/// the Browse button is held shut (`App::picking`).
const DIALOG: Duration = Duration::from_secs(600);

/// The filter rule kind the portal calls a shell glob (`0`); `1` is a
/// MIME type, which a `.zip` full of JSON is not usefully described by.
const RULE_GLOB: u32 = 0;

/// What the dialog came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// The files the user chose, in the order the portal listed them.
    /// Never empty: an empty selection is [`Picked::Cancelled`].
    Files(Vec<PathBuf>),
    /// The dialog was dismissed. Not an error, and not worth a status
    /// line that reads like one.
    Cancelled,
}

/// Open the desktop's own file dialog, multi-select, and wait for it.
///
/// Blocking: give it a thread. The error is already a sentence, because
/// the only reader is the window's status line.
pub fn pick(title: &str) -> Result<Picked, String> {
    let deadline = Instant::now() + DIALOG;
    let conn = Connection::session().map_err(|err| format!("no session bus: {err}"))?;
    let sender = conn
        .unique_name()
        .map(|name| mangle_sender(name.as_str()))
        .ok_or_else(|| "the session bus issued no unique name".to_string())?;
    let proxy = Proxy::new_owned(
        conn.clone(),
        PORTAL_BUS.to_string(),
        PORTAL_PATH.to_string(),
        FILECHOOSER_INTERFACE.to_string(),
    )
    .map_err(explain)?;

    let token = handle_token();
    let predicted = request_path(&sender, &token);
    let watch = watch_response(&conn, &predicted)?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("multiple", Value::from(true));
    options.insert("directory", Value::from(false));
    options.insert("modal", Value::from(true));
    options.insert("accept_label", Value::from("Add"));
    options.insert("filters", Value::from(filters()));
    // No parent window: exporting an xdg_toplevel handle needs
    // `xdg-foreign`, which iced does not surface. `modal` still asks the
    // portal for a dialog the user cannot lose behind the window.
    let handle: zbus::zvariant::OwnedObjectPath =
        proxy.call("OpenFile", &("", title, options)).map_err(explain)?;

    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // A portal that ignored `handle_token`: listen where the handle
        // actually is, and accept that a very fast reply may be lost -
        // the deadline is what keeps that from hanging.
        drop(watch);
        watch_response(&conn, handle.as_str())?
    };
    let (code, results) = watch.wait(deadline)?;
    match code {
        RESPONSE_SUCCESS => Ok(uris(&results)),
        RESPONSE_CANCELLED => Ok(Picked::Cancelled),
        other => Err(format!("the file dialog ended without an answer (code {other})")),
    }
}

/// The dialog's two filters: what chibipop imports, and an escape hatch
/// for an archive someone named `.ZIP` or nothing at all.
fn filters() -> Array<'static> {
    // `a(sa(us))`: a list of (label, list of (rule kind, pattern)).
    let rule = Signature::structure(vec![Signature::U32, Signature::Str]);
    let filter = Signature::structure(vec![Signature::Str, Signature::array(rule.clone())]);
    let mut filters = Array::new(&filter);
    for (label, patterns) in
        [("Yomitan archives (*.zip)", ["*.zip", "*.ZIP"].as_slice()), ("All files", &["*"])]
    {
        let mut rules = Array::new(&rule);
        for pattern in patterns {
            let entry = Structure::from((RULE_GLOB, (*pattern).to_string()));
            // Both arrays are built from the signatures just declared,
            // so a mismatch would be this function disagreeing with
            // itself; there is no runtime input to reject.
            debug_assert_eq!(&rule, entry.signature());
            let _ = rules.append(Value::from(entry));
        }
        let entry = Structure::from((label.to_string(), rules));
        debug_assert_eq!(&filter, entry.signature());
        let _ = filters.append(Value::from(entry));
    }
    filters
}

/// The `uris` result, as local paths. Anything that is not a `file://`
/// URI is dropped rather than guessed at: the portal returns those only
/// for backends chibipop cannot read from anyway.
fn uris(results: &HashMap<String, OwnedValue>) -> Picked {
    let Some(Value::Array(list)) = results.get("uris").map(|value| Value::from(value.clone()))
    else {
        return Picked::Cancelled;
    };
    let paths: Vec<PathBuf> = list
        .iter()
        .filter_map(|value| match value {
            Value::Str(uri) => file_uri_path(uri.as_str()),
            _ => None,
        })
        .collect();
    if paths.is_empty() {
        Picked::Cancelled
    } else {
        Picked::Files(paths)
    }
}

/// `file:///a/b%20c` to `/a/b c`.
///
/// Percent decoding is byte-wise on purpose: a path is bytes on this
/// platform, and a filename that is not UTF-8 still opens.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // An authority is only ever `localhost` here, and an empty one is
    // what every portal sends.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let raw = path.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' if i + 2 < raw.len() => {
                match u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).ok()?, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // A stray `%` is a legal byte in a filename.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

/// A `Request.Response` payload: the spec's `(ua{sv})`, or why we never
/// got one.
type Answer = Result<(u32, HashMap<String, OwnedValue>), String>;

/// A registered subscription to one Request's `Response`, already being
/// pumped by its own thread.
///
/// zbus's blocking iterator has no bounded wait, and a dialog nobody
/// answers must not wedge this thread forever, so the iterator goes to a
/// short-lived thread and the caller uses `recv_timeout`.
struct ResponseWatch {
    rx: mpsc::Receiver<Answer>,
}

fn watch_response(conn: &Connection, path: &str) -> Result<ResponseWatch, String> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(PORTAL_BUS)
        .and_then(|builder| builder.path(path.to_string()))
        .and_then(|builder| builder.interface(REQUEST_INTERFACE))
        .and_then(|builder| builder.member("Response"))
        .map_err(|err| format!("bad match rule for {path}: {err}"))?
        .build();
    // One Response per Request: the queue only has to outlive the gap
    // between registering and reading.
    let iterator =
        MessageIterator::for_match_rule(rule, conn, Some(2)).map_err(explain)?;

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("chibipop-filechooser-req".to_string())
        .spawn(move || {
            let answer = match iterator.into_iter().next() {
                Some(Ok(message)) => message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map_err(|err| format!("the file dialog sent a malformed answer: {err}")),
                Some(Err(err)) => Err(format!("bus error waiting on the file dialog: {err}")),
                // The iterator ended: the connection closed under us.
                None => {
                    Err("the session bus closed before the file dialog answered".to_string())
                }
            };
            let _ = tx.send(answer);
        })
        .map_err(|err| format!("no thread to wait on the file dialog: {err}"))?;

    Ok(ResponseWatch { rx })
}

impl ResponseWatch {
    /// Block until the portal answers or `deadline` passes.
    fn wait(self, deadline: Instant) -> Answer {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(remaining) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "the file dialog did not answer within {} minutes",
                DIALOG.as_secs() / 60
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("the thread waiting on the file dialog stopped".to_string())
            }
        }
    }
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

/// Unique within this process by the counter, unguessable enough by the
/// clock - the portal requires a token no other request is using.
fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chibipop_{now}_{n}")
}

/// A bus failure in the words the user's status line can use. A desktop
/// with no FileChooser portal is the one case worth naming: the typed
/// path next to the button still works, so the sentence says so.
fn explain(err: zbus::Error) -> String {
    let missing = matches!(&err, zbus::Error::MethodError(name, _, _)
        if matches!(name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"));
    if missing {
        return "This desktop has no file-chooser portal (xdg-desktop-portal is not \
                running or has no backend); type the path instead."
            .to_string();
    }
    format!("The file dialog could not be opened: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing a unit test cannot fake: whether a real
    /// xdg-desktop-portal accepts this exact `OpenFile` payload. A
    /// wrong `filters` signature is a `MethodError` here and a silent
    /// no-dialog in the window, so the call is made for real and the
    /// Request is closed again immediately - no human ever sees it.
    ///
    /// Ignored by default: it needs a session bus with a portal
    /// backend, which CI has not got.
    #[test]
    #[ignore = "needs a session bus with an xdg-desktop-portal backend"]
    fn the_real_portal_accepts_this_open_file_payload() {
        let conn = Connection::session().expect("a session bus");
        let proxy = Proxy::new_owned(
            conn.clone(),
            PORTAL_BUS.to_string(),
            PORTAL_PATH.to_string(),
            FILECHOOSER_INTERFACE.to_string(),
        )
        .expect("the FileChooser proxy");
        let version: u32 = proxy.get_property("version").expect("a FileChooser portal");
        assert!(version >= 1, "version {version}");

        let token = handle_token();
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("multiple", Value::from(true));
        options.insert("directory", Value::from(false));
        options.insert("modal", Value::from(true));
        options.insert("accept_label", Value::from("Add"));
        options.insert("filters", Value::from(filters()));
        let handle: zbus::zvariant::OwnedObjectPath = proxy
            .call("OpenFile", &("", "Add dictionary archives", options))
            .expect("the portal to accept the payload");

        // Whatever path it chose, that is where the dialog is; close it.
        let request = Proxy::new_owned(
            conn,
            PORTAL_BUS.to_string(),
            handle.as_str().to_string(),
            REQUEST_INTERFACE.to_string(),
        )
        .expect("the Request proxy");
        let _: zbus::Result<()> = request.call("Close", &());
    }

    #[test]
    fn a_percent_escaped_file_uri_becomes_the_path_it_names() {
        assert_eq!(
            Some(PathBuf::from("/home/a/My Dicts/jitendex.zip")),
            file_uri_path("file:///home/a/My%20Dicts/jitendex.zip")
        );
    }

    #[test]
    fn a_localhost_authority_is_the_same_path() {
        assert_eq!(
            Some(PathBuf::from("/srv/terms.zip")),
            file_uri_path("file://localhost/srv/terms.zip")
        );
    }

    #[test]
    fn a_percent_that_escapes_nothing_stays_a_percent() {
        // A legal byte in a filename, and a portal is free to send it
        // unescaped; dropping the file would be the worse answer.
        assert_eq!(Some(PathBuf::from("/tmp/100%.zip")), file_uri_path("file:///tmp/100%.zip"));
    }

    #[test]
    fn a_non_file_uri_is_dropped_rather_than_guessed_at() {
        // `stage_add` would only report it unreadable one screen later.
        assert_eq!(None, file_uri_path("smb://nas/share/terms.zip"));
        assert_eq!(None, file_uri_path("file:relative.zip"));
    }

    #[test]
    fn the_filter_list_carries_the_signature_the_portal_declares() {
        // `a(sa(us))` per FileChooser.xml; `Array::append` refuses a
        // mismatch, so an empty list here would be the bug.
        assert_eq!("a(sa(us))", filters().signature().to_string());
        assert_eq!(2, filters().len());
    }

    #[test]
    fn an_empty_uri_list_reads_as_a_dismissed_dialog() {
        // A portal answering success with nothing selected: staging zero
        // archives and saying "added" would be a lie.
        let mut results = HashMap::new();
        let empty = Array::new(&Signature::Str);
        results.insert("uris".to_string(), OwnedValue::try_from(Value::from(empty)).unwrap());
        assert_eq!(Picked::Cancelled, uris(&results));
    }

    #[test]
    fn every_chosen_uri_comes_back_in_the_order_the_portal_listed_it() {
        let mut list = Array::new(&Signature::Str);
        list.append(Value::from("file:///a/one.zip")).unwrap();
        list.append(Value::from("file:///a/two.zip")).unwrap();
        let mut results = HashMap::new();
        results.insert("uris".to_string(), OwnedValue::try_from(Value::from(list)).unwrap());
        assert_eq!(
            Picked::Files(vec![PathBuf::from("/a/one.zip"), PathBuf::from("/a/two.zip")]),
            uris(&results)
        );
    }

    #[test]
    fn two_tokens_are_never_the_same_request_path() {
        assert_ne!(handle_token(), handle_token());
        assert_eq!(
            "/org/freedesktop/portal/desktop/request/1_23/tok",
            request_path(&mangle_sender(":1.23"), "tok")
        );
    }
}
