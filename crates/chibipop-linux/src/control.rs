//! The control socket (ARCHITECTURE.md#input-ladders) uses a UNIX socket at
//! `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.sock`. It uses the same key as the
//! instance lock, so both names identify one instance. The socket accepts the minimal
//! forever verb set.
//!
//! **One verb per global action, and nothing else.** A verb exists only when it names
//! an action that a user can bind to a key. No verb reads state, takes an argument, or
//! combines actions. This transports actions from compositor binds. Portal presses use the same
//! core action path. It is not an API for scripts. The settings window reads status through
//! `shortcuts::state` for this reason.
//!
//! Wire format: one request line (`trigger-down\n`) and one reply line
//! (`OK …` or `ERR …`). `bindsym` lines start `chibipop ctl` as a child
//! process. A human can also use `nc -U`.

use crate::lock::sanitize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The minimal forever verb set. It has one verb per global action and is
/// not an API for scripts (ARCHITECTURE.md#input-ladders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Reload,
    TriggerDown,
    TriggerUp,
    Toggle,
    /// Run one lookup at the cursor with a live grab, as
    /// [`chibipop::config::TriggerMode::Press`] does. The popup stays until a
    /// later lookup finds no text or the user clicks outside it. The verb
    /// takes no hold and no frozen grab, so a press over the popup is a miss.
    Lookup,
    /// Add a card for the lookup on screen. This is the same action that the
    /// portal `anki-add` shortcut performs. Both channels use this wire name.
    AnkiAdd,
    /// Grab a region and save it as the mining context for the lookup on screen
    /// (`actions.screenshot`). This action uses only the native channel for the
    /// same reason as `static-region`.
    Screenshot,
    /// Pick a region, run OCR, and place the text on the clipboard
    /// (`actions.ocr_clipboard`). This action uses only the native channel for
    /// the same reason as `static-region`.
    OcrClipboard,
    /// Draw the box that [`chibipop::config::SentenceMode::Static`] reads for
    /// the Anki sentence. This action uses only the native channel. The portal
    /// identifier set has exactly two members, so this verb is the action's only
    /// global channel.
    StaticRegion,
}

pub const VERBS: [Verb; 9] = [
    Verb::Reload,
    Verb::TriggerDown,
    Verb::TriggerUp,
    Verb::Toggle,
    Verb::Lookup,
    Verb::AnkiAdd,
    Verb::Screenshot,
    Verb::OcrClipboard,
    Verb::StaticRegion,
];

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Reload => "reload",
            Verb::TriggerDown => "trigger-down",
            Verb::TriggerUp => "trigger-up",
            Verb::Toggle => "toggle",
            Verb::Lookup => "lookup",
            Verb::AnkiAdd => "anki-add",
            Verb::Screenshot => "screenshot",
            Verb::OcrClipboard => "ocr-clipboard",
            Verb::StaticRegion => "static-region",
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
            // Do not count this action. A picture depends on the region pick and on
            // whether AnkiConnect can accept a card. A counter would not report that
            // result. This line reports the request, not the result.
            Verb::Screenshot => "picking the mining screenshot's region".to_string(),
            // Do not count this action. The pick, the grab, and the OCR engine can each
            // fail. The compositor can lack a clipboard protocol. This line reports the
            // request.
            Verb::OcrClipboard => "picking a region to OCR onto the clipboard".to_string(),
            // Do not count this action. The pick decides whether a region exists. A
            // cancel, a drag below the threshold, or no layer shell can leave no region.
            // This line reports the request, not the result.
            Verb::StaticRegion => "picking the static sentence region".to_string(),
        }
    }
}

/// The daemon's socket endpoint. It removes the socket file when dropped.
pub struct ControlSocket {
    listener: UnixListener,
    path: PathBuf,
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
        Ok(ControlSocket { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    /// Serve every connection that the listener has queued. Return one
    /// `(reply_sent, verb_if_valid)` pair per connection. The caller logs and
    /// applies each verb, so this method does not own daemon state.
    pub fn drain(&self) -> Vec<(String, Option<Verb>)> {
        let mut served = Vec::new();
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    if let Some(outcome) = serve_one(stream) {
                        served.push(outcome);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
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

/// Read one request line and write one reply line.
fn serve_one(stream: UnixStream) -> Option<(String, Option<Verb>)> {
    // Serve this local, short-lived stream in a mode that blocks, with a deadline.
    // The daemon must not wait forever for one stuck client.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let request = line.trim();

    let (reply, verb) = match Verb::parse(request) {
        Some(verb) => (format!("OK {request}\n"), Some(verb)),
        None => (format!("ERR unknown verb {request:?}; expected one of {}\n", verb_list()), None),
    };
    let mut stream = reader.into_inner();
    let _ = stream.write_all(reply.as_bytes());
    Some((request.to_string(), verb))
}

/// Send one verb through the socket and return the daemon's reply.
pub fn send(runtime_dir: &Path, display: &str, verb: Verb) -> std::io::Result<String> {
    send_to(&runtime_dir.join(file_name(display)), verb)
}

/// Send the same exchange through a socket path that the caller already has.
///
/// The settings process owns this path. In `ApplyMode`, a connectable path means
/// live apply. An absent path means config-only. This function must not derive
/// the path again.
pub fn send_to(path: &Path, verb: Verb) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    // Allow startup time: the worker pipeline can load its model for seconds on
    // slow hardware. The pump answers when it runs. The `write_all` call sends the
    // verb, so a real key press must outwait this period and avoid the timeout.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(format!("{}\n", verb.as_str()).as_bytes())?;
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
            "reload, trigger-down, trigger-up, toggle, lookup, anki-add, screenshot, \
             ocr-clipboard, static-region",
            verb_list()
        );
    }

    /// `anki-add` matches the portal shortcut ID. Rung 1 and rung 2 therefore
    /// name one action, not two.
    #[test]
    fn the_add_verb_and_the_portal_shortcut_id_share_one_name() {
        assert_eq!(crate::shortcuts::ShortcutId::AnkiAdd.as_str(), Verb::AnkiAdd.as_str());
    }

    /// D1 is a property, not a review habit. The `static-region` action has a
    /// verb but no portal ID. The consent dialog therefore does not include this
    /// action. A native bind is its only global channel, as the settings row
    /// caption states.
    #[test]
    fn the_static_region_verb_is_native_channel_only() {
        assert_eq!(2, crate::shortcuts::ShortcutId::ALL.len(), "the portal id set is closed");
        assert!(
            !crate::shortcuts::ShortcutId::ALL
                .iter()
                .any(|id| id.as_str() == Verb::StaticRegion.as_str()),
            "no portal id may name the static-region action"
        );
    }

    /// OCR-to-clipboard has the same property: it has a verb but no portal ID.
    #[test]
    fn the_ocr_clipboard_verb_is_native_channel_only() {
        assert!(
            !crate::shortcuts::ShortcutId::ALL
                .iter()
                .any(|id| id.as_str() == Verb::OcrClipboard.as_str()),
            "no portal id may name the OCR-to-clipboard action"
        );
    }

    #[test]
    fn an_unknown_verb_does_not_parse() {
        assert_eq!(None, Verb::parse("open-settings"));
        assert_eq!(None, Verb::parse(""));
        assert_eq!(None, Verb::parse("TRIGGER-DOWN"));
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
        assert_eq!(vec![("trigger-down".to_string(), Some(Verb::TriggerDown))], served);
        assert_eq!("OK trigger-down", client.join().unwrap().expect("client reply"));

        let path = socket.path().to_path_buf();
        drop(socket);
        assert!(!path.exists(), "socket file must be unlinked on drop");
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
