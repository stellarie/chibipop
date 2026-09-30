//! The control socket (ARCHITECTURE.md#input-ladders) uses a UNIX socket at
//! `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.sock`. It uses the same key as the
//! instance lock, so both names identify one instance. The socket accepts the minimal
//! forever verb set.
//!
//! Wire format: one request line (`trigger-down\n`) and one reply line
//! (`OK …` or `ERR …`). `bindsym` lines start `chibipop ctl` as a child
//! process. A human can also use `nc -U`.

use crate::lock::sanitize;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 256;

/// One accepted control-socket request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    Verb(Verb),
    BindId { id: String, activated: bool },
}

/// The minimal forever verb set. It has one verb per global action and is
/// not an API for scripts (ARCHITECTURE.md#input-ladders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Search,
    SentenceSearch,
    Reload,
    TriggerDown,
    TriggerUp,
    Toggle,
    /// Run one lookup at the cursor with a live grab, as
    /// [`chibipop::config::TriggerMode::Press`] does. The popup stays until a
    /// later lookup finds no text or the user clicks outside it. The verb
    /// takes no hold and no frozen grab, so a press over the popup is a miss.
    Lookup,
    AnkiAdd,
    OcrClipboard,
    StaticRegion,
    SelectedText,
}

pub const VERBS: [Verb; 11] = [
    Verb::Reload,
    Verb::TriggerDown,
    Verb::TriggerUp,
    Verb::Toggle,
    Verb::Lookup,
    Verb::AnkiAdd,
    Verb::OcrClipboard,
    Verb::StaticRegion,
    Verb::Search,
    Verb::SentenceSearch,
    Verb::SelectedText,
];

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Search => "search",
            Verb::SentenceSearch => "sentence-search",
            Verb::Reload => "reload",
            Verb::TriggerDown => "trigger-down",
            Verb::TriggerUp => "trigger-up",
            Verb::Toggle => "toggle",
            Verb::Lookup => "lookup",
            Verb::AnkiAdd => "anki-add",
            Verb::OcrClipboard => "ocr-clipboard",
            Verb::StaticRegion => "static-region",
            Verb::SelectedText => "selected-text",
        }
    }

    pub fn parse(text: &str) -> Option<Verb> {
        VERBS.into_iter().find(|v| v.as_str() == text)
    }
}

/// One socket per compositor instance, beside its lock.
pub fn file_name(display: &str) -> String {
    format!("run-{}.sock", sanitize(display))
}

/// State from the received verbs. This placeholder remains until the core
/// `Controller` handles these verbs. Each verb updates this state and returns
/// a diagnostic line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StubState {
    pub reloads: u32,
    pub trigger_held: bool,
    pub toggled_on: bool,
}

impl StubState {
    /// Apply one verb. The returned line serves as the log entry's tail and
    /// the `OK` reply's tail.
    pub fn apply(&mut self, verb: Verb) -> String {
        match verb {
            Verb::Search => "opening dictionary search".to_string(),
            Verb::SentenceSearch => "opening sentence search".to_string(),
            Verb::Reload => {
                self.reloads += 1;
                format!("reload #{} requested", self.reloads)
            }
            Verb::TriggerDown => {
                self.trigger_held = true;
                "trigger held".to_string()
            }
            Verb::TriggerUp => {
                self.trigger_held = false;
                "trigger released".to_string()
            }
            Verb::Toggle => {
                self.toggled_on = !self.toggled_on;
                format!("toggled {}", if self.toggled_on { "on" } else { "off" })
            }
            // Do not count this action. The lookup can miss, and the Controller
            // decides what stays on screen. This line reports the request.
            Verb::Lookup => "lookup requested at the cursor".to_string(),
            // Do not count this action. The daemon's `Controller` decides whether an
            // add occurs at all. It considers cases with no card, an empty expression, and
            // an already added card. A counter here would provide a second, less accurate
            // answer. This line reports the request.
            Verb::AnkiAdd => "card requested for the lookup on screen".to_string(),
            // Do not count this action. The pick, the grab, and the OCR engine can each
            // fail. The compositor can lack a clipboard protocol. This line reports the
            // request.
            Verb::OcrClipboard => "picking a region to OCR onto the clipboard".to_string(),
            // Do not count this action. The pick decides whether a region exists. A
            // cancel, a drag below the threshold, or no layer shell can leave no region.
            // This line reports the request, not the result.
            Verb::StaticRegion => "picking the static sentence region".to_string(),
            Verb::SelectedText => "reading selected text for lookup".to_string(),
        }
    }
}

/// Shared admission state for configured bind-ID requests.
#[derive(Debug, Clone, Default)]
pub struct ControlPolicy {
    bind_ids: Arc<RwLock<HashSet<String>>>,
}

impl ControlPolicy {
    /// Replace the configured enabled bind IDs accepted by this socket.
    pub fn set_bind_ids(&self, ids: impl IntoIterator<Item = String>) {
        let ids = ids.into_iter().filter(|id| valid_bind_id(id)).collect();
        *self.bind_ids.write().unwrap_or_else(std::sync::PoisonError::into_inner) = ids;
    }
}

/// The daemon's socket endpoint. It removes the socket file when dropped.
pub struct ControlSocket {
    listener: UnixListener,
    path: PathBuf,
    policy: ControlPolicy,
}

impl ControlSocket {
    /// Bind the socket and replace a stale socket file. The instance lock allows
    /// only one daemon. Therefore, this file can only belong to an earlier daemon,
    /// for example after `SIGKILL`.
    pub fn bind(runtime_dir: &Path, display: &str) -> std::io::Result<ControlSocket> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join(file_name(display));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        // The calloop source polls this listener. `accept` must not block the pump.
        listener.set_nonblocking(true)?;
        Ok(ControlSocket { listener, path, policy: ControlPolicy::default() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    /// Return shared admission state for use by the daemon reload path.
    pub fn policy(&self) -> ControlPolicy {
        self.policy.clone()
    }

    /// Replace the configured enabled bind IDs accepted by this socket.
    pub fn set_bind_ids(&mut self, ids: impl IntoIterator<Item = String>) {
        self.policy.set_bind_ids(ids);
    }

    /// Serve every queued connection. Each result contains its reply text and
    /// an accepted request, if the request passed validation.
    pub fn drain(&self) -> Vec<(String, Option<ControlRequest>)> {
        let mut served = Vec::new();
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    if let Some(outcome) = serve_one(stream, &self.policy) {
                        served.push(outcome);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        served
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        // A dead socket file causes harm unlike a lock file. The next daemon must
        // bind, and `ctl` must receive "no such file" instead of a connection to a
        // stale socket.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read one bounded request line and write one reply line.
fn serve_one(
    stream: UnixStream,
    policy: &ControlPolicy,
) -> Option<(String, Option<ControlRequest>)> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .by_ref()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_line(&mut line)
        .ok()?;
    let request = line.trim();
    let mut stream = reader.into_inner();

    if line.len() > MAX_REQUEST_BYTES || !line.ends_with('\n') {
        let _ = stream.write_all(b"ERR request must be one line of at most 256 bytes\n");
        return Some((request.to_string(), None));
    }

    let bind_ids = policy.bind_ids.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (reply, parsed) = match parse_request(request, &bind_ids) {
        Ok(parsed) => (format!("OK {request}\n"), Some(parsed)),
        Err(reply) => (format!("ERR {reply}\n"), None),
    };
    drop(bind_ids);
    let _ = stream.write_all(reply.as_bytes());
    Some((request.to_string(), parsed))
}

fn parse_request(request: &str, bind_ids: &HashSet<String>) -> Result<ControlRequest, String> {
    if let Some(verb) = Verb::parse(request) {
        return Ok(ControlRequest::Verb(verb));
    }

    let mut words = request.split_whitespace();
    let verb = words.next();
    let id = words.next();
    let extra = words.next();
    if matches!(verb, Some("bind-down" | "bind-up")) {
        let Some(id) = id else {
            return Err("expected bind-down <id> or bind-up <id>".to_string());
        };
        if extra.is_some() || !valid_bind_id(id) {
            return Err("malformed bind ID".to_string());
        }
        if !bind_ids.contains(id) {
            return Err(format!("bind ID {id:?} is not configured or enabled"));
        }
        return Ok(ControlRequest::BindId {
            id: id.to_string(),
            activated: verb == Some("bind-down"),
        });
    }

    Err(format!(
        "unknown verb {request:?}; expected one of {} or bind-down <id>, bind-up <id>",
        verb_list()
    ))
}

fn valid_bind_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Send one fixed verb through the socket and return the daemon's reply.
pub fn send(runtime_dir: &Path, display: &str, verb: Verb) -> std::io::Result<String> {
    send_to(&runtime_dir.join(file_name(display)), verb)
}

/// Send one configured bind activation or release through the socket.
pub fn send_bind(
    runtime_dir: &Path,
    display: &str,
    id: &str,
    activated: bool,
) -> std::io::Result<String> {
    send_bind_to(&runtime_dir.join(file_name(display)), id, activated)
}

/// Send one configured bind event through an existing socket path.
pub fn send_bind_to(path: &Path, id: &str, activated: bool) -> std::io::Result<String> {
    if !valid_bind_id(id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "malformed bind ID"));
    }
    let verb = if activated { "bind-down" } else { "bind-up" };
    send_request_to(path, &format!("{verb} {id}\n"))
}

/// Send the same exchange through a socket path that the caller already has.
///
/// The settings process owns this path. In `ApplyMode`, a connectable path means
/// live apply. An absent path means config-only. This function must not derive
/// the path again.
pub fn send_to(path: &Path, verb: Verb) -> std::io::Result<String> {
    send_request_to(path, &format!("{}\n", verb.as_str()))
}

fn send_request_to(path: &Path, request: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(request.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

pub fn verb_list() -> String {
    VERBS.map(Verb::as_str).join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_round_trips_through_its_wire_name() {
        for verb in VERBS {
            assert_eq!(Some(verb), Verb::parse(verb.as_str()));
        }
    }

    /// These literal names form the contract. A bind line that a user pasted
    /// years ago must remain valid. A rename is a breaking change, so this
    /// assertion records the rule. An addition at the end keeps every old name.
    #[test]
    fn the_wire_names_are_the_forever_contract() {
        assert_eq!(
            "reload, trigger-down, trigger-up, toggle, lookup, anki-add, \
             ocr-clipboard, static-region, search, sentence-search, selected-text",
            verb_list()
        );
    }

    #[test]
    fn configured_bind_requests_require_a_valid_enabled_id() {
        let enabled = HashSet::from(["bind-7".to_string()]);
        assert_eq!(
            Ok(ControlRequest::BindId { id: "bind-7".into(), activated: true }),
            parse_request("bind-down bind-7", &enabled),
        );
        assert_eq!(
            Ok(ControlRequest::BindId { id: "bind-7".into(), activated: false }),
            parse_request("bind-up bind-7", &enabled),
        );
        assert_eq!(
            Err("malformed bind ID".to_string()),
            parse_request("bind-down bad/id", &enabled),
        );
        assert!(parse_request("bind-down missing", &enabled).unwrap_err().contains("not configured"));
        assert!(parse_request("bind-down bind-7 extra", &enabled).unwrap_err().contains("malformed"));
    }

    #[test]
    fn all_saved_bind_ids_use_the_config_id_grammar() {
        assert!(valid_bind_id("bind_02"));
        assert!(!valid_bind_id(""));
        assert!(!valid_bind_id("bad/id"));
        assert!(!valid_bind_id(&"x".repeat(65)));
    }

    #[test]
    fn an_unknown_verb_does_not_parse() {
        assert_eq!(None, Verb::parse("open-settings"));
        assert_eq!(None, Verb::parse(""));
        assert_eq!(None, Verb::parse("TRIGGER-DOWN"));
        assert_eq!(None, Verb::parse("screenshot"));
    }

    #[test]
    fn the_stub_state_tracks_hold_and_toggle() {
        let mut state = StubState::default();
        state.apply(Verb::TriggerDown);
        assert!(state.trigger_held);
        state.apply(Verb::TriggerUp);
        assert!(!state.trigger_held);
        state.apply(Verb::Toggle);
        assert!(state.toggled_on);
        state.apply(Verb::Reload);
        state.apply(Verb::Reload);
        assert_eq!(2, state.reloads);
    }

    /// This test does a bind and connect roundtrip without a compositor in a
    /// temporary directory.
    #[test]
    fn a_verb_round_trips_over_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("chibipop_ctl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = ControlSocket::bind(&dir, "test-0").expect("bind");

        let dir2 = dir.clone();
        let client = std::thread::spawn(move || send(&dir2, "test-0", Verb::TriggerDown));

        // Poll the nonblocking listener until the client sends its request.
        let mut served = Vec::new();
        for _ in 0..200 {
            served = socket.drain();
            if !served.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            vec![("trigger-down".to_string(), Some(ControlRequest::Verb(Verb::TriggerDown)))],
            served
        );
        assert_eq!("OK trigger-down", client.join().unwrap().expect("client reply"));

        let path = socket.path().to_path_buf();
        drop(socket);
        assert!(!path.exists(), "socket file must be unlinked on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_bind_id_round_trips_over_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("chibipop_bind_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut socket = ControlSocket::bind(&dir, "test-2").expect("bind");
        socket.set_bind_ids(["bind-7".to_string()]);
        let path = socket.path().to_path_buf();

        let client = std::thread::spawn(move || send_bind_to(&path, "bind-7", true));
        let mut served = Vec::new();
        for _ in 0..200 {
            served = socket.drain();
            if !served.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            vec![(
                "bind-down bind-7".to_string(),
                Some(ControlRequest::BindId { id: "bind-7".into(), activated: true }),
            )],
            served,
        );
        assert_eq!("OK bind-down bind-7", client.join().unwrap().expect("client reply"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shared_policy_rejects_bind_ids_retired_on_reload() {
        let dir = std::env::temp_dir().join(format!("chibipop_bind_reload_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut socket = ControlSocket::bind(&dir, "test-reload").expect("bind");
        socket.set_bind_ids(["retired-8".to_string()]);
        socket.policy().set_bind_ids(Vec::<String>::new());
        let path = socket.path().to_path_buf();

        let client = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            stream.write_all(b"bind-down retired-8\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        });
        let mut served = Vec::new();
        for _ in 0..200 {
            served = socket.drain();
            if !served.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(vec![("bind-down retired-8".to_string(), None)], served);
        assert!(client.join().unwrap().starts_with("ERR bind ID"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unconfigured_bind_id_gets_an_err_reply_before_dispatch() {
        let dir = std::env::temp_dir().join(format!("chibipop_bind_err_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = ControlSocket::bind(&dir, "test-3").expect("bind");
        let path = socket.path().to_path_buf();

        let client = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            stream.write_all(b"bind-down missing\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        });
        let mut served = Vec::new();
        for _ in 0..200 {
            served = socket.drain();
            if !served.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(vec![("bind-down missing".to_string(), None)], served);
        assert!(client.join().unwrap().starts_with("ERR bind ID"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reply channel rejects invalid input without stopping the daemon.
    #[test]
    fn an_unknown_verb_gets_an_err_reply() {
        let dir = std::env::temp_dir().join(format!("chibipop_ctl_err_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = ControlSocket::bind(&dir, "test-1").expect("bind");
        let path = socket.path().to_path_buf();

        let client = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            stream.write_all(b"frobnicate\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        });

        let mut served = Vec::new();
        for _ in 0..200 {
            served = socket.drain();
            if !served.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(vec![("frobnicate".to_string(), None)], served);
        let reply = client.join().unwrap();
        assert!(reply.starts_with("ERR unknown verb"), "{reply}");
        assert!(reply.contains("trigger-down"), "the ERR must teach the verb set: {reply}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
