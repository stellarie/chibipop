//! `org.freedesktop.portal.FileChooser`: the Browse button in the Add row.
//!
//! **Why the portal.** The settings window is a Wayland client without a
//! toolkit file dialog. It stays iced-native. `OpenFile` is the one dialog
//! that each supported desktop implements. KDE draws Plasma's dialog, and
//! GNOME draws GTK's dialog. A Browse button needs one D-Bus call instead of
//! another widget stack. The portal also works from a Flatpak. Its document
//! store returns real paths under `/run/user`.
//!
//! **Why blocking zbus on a borrowed thread.** This matches the capture and
//! shortcuts portals ([`crate::shortcuts::portal`]). zbus already exists in
//! the tree with a blocking API, so no async runtime is needed. A dialog waits
//! for a person. Do not make this call on iced's executor. The caller gives
//! [`pick`] its own thread, and the channel returns one answer.
//!
//! **The Request race** uses this order.
//! The capture and shortcuts portals use the same order. The Request object
//! path is predictable (`…/request/<SENDER>/<token>`), so add the match rule
//! before the call. A portal can retain a previous answer and reply before a
//! subscription that starts after the call can receive it.
//!
//! The pinned signature table comes from interface version 4, not memory.
//! The reference file is
//! `/usr/share/dbus-1/interfaces/org.freedesktop.portal.FileChooser.xml`.
//! `OpenFile` takes `(parent_window: s, title: s, options: a{sv})` and returns `o`.
//! Its `Response` carries `uris` as an array of strings.

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
/// The portal's only object path. Every portal interface uses it.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The interface that this module calls.
const FILECHOOSER_INTERFACE: &str = "org.freedesktop.portal.FileChooser";
/// Object path for a portal method's deferred response.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// `Response` code 0 means success. Code 1 means that the user dismissed the dialog.
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// Allow a person 600 seconds to choose files.
/// No other operation blocks. The window continues to render, and only the
/// Browse button stays disabled (`App::picking`).
const DIALOG: Duration = Duration::from_secs(600);

/// Filter rule kind for a shell glob (`0`).
/// Value `1` means a MIME type. MIME identifies a ZIP container.
/// It cannot distinguish Yomitan ZIP contents from other ZIP contents.
const RULE_GLOB: u32 = 0;

/// Paths that the user chose, in portal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// Never empty. An empty selection returns [`Picked::Cancelled`].
    Files(Vec<PathBuf>),
    /// The user dismissed the dialog. This is not an error, so the status line
    /// does not report one.
    Cancelled,
}

/// Open the desktop file dialog.
/// The dialog allows multiple files.
/// This call waits for an answer.
///
/// This call blocks. The caller must give it a thread.
/// The error text is already a sentence for the window status line.
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
    // No parent window. An `xdg_toplevel` handle requires `xdg-foreign`, which
    // iced does not expose. `modal` still asks the portal for a dialog that
    // the user cannot lose behind the window.
    let handle: zbus::zvariant::OwnedObjectPath =
        proxy.call("OpenFile", &("", title, options)).map_err(explain)?;

    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // If the portal ignores `handle_token`, listen at the actual handle path.
        // A very fast reply can escape this watcher. The deadline prevents an
        // indefinite wait.
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

/// Define two dialog filters. The first accepts archives that chibipop imports.
/// The second accepts all files. It also accepts an archive with `.ZIP` or no extension.
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
            // Build both arrays from the declared signatures.
            // A mismatch would mean that this function disagrees with itself.
            // Runtime input cannot cause this mismatch.
            debug_assert_eq!(&rule, entry.signature());
            let _ = rules.append(Value::from(entry));
        }
        let entry = Structure::from((label.to_string(), rules));
        debug_assert_eq!(&filter, entry.signature());
        let _ = filters.append(Value::from(entry));
    }
    filters
}

/// Convert the `uris` result to local paths.
/// Keep only `file://` URIs because `stage_add` needs a local path.
/// Drop URI values with any other scheme.
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

/// Convert `file:///a/b%20c` to `/a/b c`.
///
/// Decode each percent escape to one byte.
/// Build the `PathBuf` from the resulting bytes. This preserves non-UTF-8 file names.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // The authority is `localhost` or empty for this portal.
    // The portal sends the empty form.
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
                    // A stray `%` is a legal filename byte.
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

/// A `Request.Response` payload with the spec's `(ua{sv})` format.
/// The result can also explain why no payload arrived.
type Answer = Result<(u32, HashMap<String, OwnedValue>), String>;

/// A subscription to one Request's `Response`.
/// Its thread reads messages from the bus.
///
/// The zbus blocking iterator has no bounded wait.
/// A dialog with no answer can keep its reader thread waiting.
/// Let the caller use `recv_timeout` for the deadline.
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
    // One Response exists for each Request. The iterator keeps its message queue
    // from registration until the reader receives the first Response or the connection closes.
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
                // The iterator ended because the connection closed.
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
    /// Wait until the portal answers or `deadline` passes.
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

/// Convert the unique bus name to an object-path element.
/// Remove `:` at the start.
/// Replace each `.` with `_`, as the Request docs specify.
fn mangle_sender(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

/// Return the object path where the portal puts the Request for `token`.
fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Make a token unique in this process with a counter and hard to guess from the clock.
/// The portal requires a token that no other request uses.
fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chibipop_{now}_{n}")
}

/// Convert a bus failure to text for the user status line.
/// A desktop without a FileChooser portal needs a specific message.
/// The typed path beside the button still works, so the message says so.
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

    /// A unit test cannot fake whether a real xdg-desktop-portal accepts this
    /// exact `OpenFile` payload. A wrong `filters` signature returns a
    /// `MethodError` here. Without this test, the window could silently show
    /// no dialog. Call the real portal.
    /// Close the Request immediately.
    /// No person sees the dialog.
    ///
    /// Ignore by default. This test needs a session bus with a portal backend.
    /// CI has none.
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

        // The chosen handle identifies the dialog. Close its Request immediately.
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
        // A portal can send this legal filename byte without an escape.
        // If code drops the file, the result is worse.
        assert_eq!(Some(PathBuf::from("/tmp/100%.zip")), file_uri_path("file:///tmp/100%.zip"));
    }

    #[test]
    fn a_non_file_uri_is_dropped_rather_than_guessed_at() {
        // `stage_add` would report this path as unreadable one screen later.
        assert_eq!(None, file_uri_path("smb://nas/share/terms.zip"));
        assert_eq!(None, file_uri_path("file:relative.zip"));
    }

    #[test]
    fn the_filter_list_carries_the_signature_the_portal_declares() {
        // `a(sa(us))` per FileChooser.xml.
        // `Array::append` rejects a mismatch. An empty list here would expose the bug.
        assert_eq!("a(sa(us))", filters().signature().to_string());
        assert_eq!(2, filters().len());
    }

    #[test]
    fn an_empty_uri_list_reads_as_a_dismissed_dialog() {
        // Do not stage zero archives after success with no selected files.
        // Do not report "added" after success with no selected files.
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
