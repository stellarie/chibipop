//! One blocking `org.freedesktop.portal.Request` round trip. Three portals use
//! it: ScreenCast, GlobalShortcuts, and FileChooser.
//!
//! **How this module prevents the Request race.** The method reply contains an
//! `o` Request handle. A later `Response` (`(ua{sv})`) signal on that object
//! contains the result. Code that subscribes after the method reply can miss a
//! fast `Response`. A restored session can send this signal without a dialog.
//! xdg-desktop-portal gives each handle this path:
//! `/org/freedesktop/portal/desktop/request/<SENDER>/<handle_token>`.
//! The portal derives `<SENDER>` from the unique D-Bus name. It removes the
//! first `:` and replaces each `.` with `_`. The match rule starts at the
//! predicted path before the method call. If an older portal returns another
//! path, the call subscribes to the returned path.
//!
//! **Why this module uses a waiter thread.** The zbus signal iterator has no
//! wait limit. One deadline covers the full request, so an unanswered dialog
//! cannot block the caller without end. Each wait gives its iterator to a new
//! thread. The thread sends the first `Response` through an `mpsc` channel.
//! The caller uses `recv_timeout`. If the request stops, the caller can close
//! the shared connection. This stops the iterator and its thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Instant, SystemTime};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::MatchRule;

/// The portal's well-known D-Bus name.
pub const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
/// The object path that contains all portal interfaces.
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The interface that sends the deferred response for each portal method.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
/// `Response` code 0 means that the portal completed the request.
pub const RESPONSE_SUCCESS: u32 = 0;
/// `Response` code 1 means that the user canceled the request.
pub const RESPONSE_CANCELLED: u32 = 1;
/// `Response` code 2 means that the portal ended the request.
pub const RESPONSE_ENDED: u32 = 2;

pub struct Response {
    pub code: u32,
    pub results: HashMap<String, OwnedValue>,
}

pub enum Failure {
    /// The method call itself failed. The caller classifies the zbus error.
    Call(zbus::Error),
    /// No Response before `deadline`.
    TimedOut,
    /// Match rule, thread, bus, or body shape problems. Text is the detail.
    Protocol(String),
}

pub fn mangle_sender(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

pub fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Creates a fresh `handle_token`. The token is a valid object-path element.
/// The counter and clock make it unique within this process. The code needs
/// no `rand` dependency. The token prevents collisions with other libraries
/// on the same connection. It is not a secret.
pub fn handle_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    format!("chibipop_{}_{seq}_{nanos}", std::process::id())
}

/// Calls one portal method after it registers the `Response` subscription.
/// The code predicts the Request path and registers the match rule before
/// the method call.
///
/// `call` receives the `handle_token` and returns the portal Request handle.
pub fn request(
    conn: &Connection,
    sender: &str,
    step: &'static str,
    deadline: Instant,
    call: impl FnOnce(&str) -> zbus::Result<OwnedObjectPath>,
) -> Result<Response, Failure> {
    let token = handle_token();
    let predicted = request_path(sender, &token);
    // Register the watch before the call. A restored session can answer instantly.
    let watch = watch_response(conn, &predicted, step)?;

    let handle = call(&token).map_err(Failure::Call)?;
    let watch = if handle.as_str() == predicted {
        watch
    } else {
        // A portal can ignore `handle_token`. Listen at the path that the handle
        // returns. A very fast reply can already be lost. The deadline prevents
        // an endless wait. The abandoned iterator thread ends when the caller
        // closes the connection.
        drop(watch);
        watch_response(conn, handle.as_str(), step)?
    };

    let (code, results) = watch.wait(step, deadline)?;
    Ok(Response { code, results })
}

type Answer = Result<(u32, HashMap<String, OwnedValue>), Failure>;

/// A registered subscription to one Request's `Response`. A separate thread
/// reads its messages.
struct ResponseWatch {
    rx: Receiver<Answer>,
}

/// Registers the match rule for `Response` at `path` and starts the reader
/// thread. The function returns after the bus adds the rule. The caller can
/// issue the method only after this point.
fn watch_response(conn: &Connection, path: &str, step: &'static str) -> Result<ResponseWatch, Failure> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(PORTAL_BUS)
        .and_then(|builder| builder.path(path.to_string()))
        .and_then(|builder| builder.interface(REQUEST_INTERFACE))
        .and_then(|builder| builder.member("Response"))
        .map_err(|err| Failure::Protocol(format!("{step}: bad match rule for {path}: {err}")))?
        .build();
    // One Response exists for each Request. The queue only needs to outlive the
    // interval between registration and the first read.
    let iterator = MessageIterator::for_match_rule(rule, conn, Some(2)).map_err(|err| {
        Failure::Protocol(format!("{step}: could not watch {path}: {err}"))
    })?;

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
                    .map_err(|err| Failure::Protocol(format!("{step}: malformed Response: {err}"))),
                Some(Err(err)) => {
                    Err(Failure::Protocol(format!("{step}: bus error waiting: {err}")))
                }
                // The iterator ended because the connection closed. This ends an abandoned
                // wait.
                None => Err(Failure::Protocol(format!(
                    "{step}: the session bus closed before the portal answered"
                ))),
            };
            let _ = tx.send(answer);
        })
        .map_err(|err| Failure::Protocol(format!("{step}: no thread for the wait: {err}")))?;

    Ok(ResponseWatch { rx })
}

impl ResponseWatch {
    /// Wait until the portal answers or `deadline` passes.
    fn wait(self, step: &'static str, deadline: Instant) -> Result<(u32, HashMap<String, OwnedValue>), Failure> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(remaining) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(Failure::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(Failure::Protocol(format!(
                "{step}: the waiting thread stopped without answering"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
