//! Control socket (ADR-0003): a UNIX socket at
//! `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.sock` — keyed exactly
//! like the instance lock, so lock and socket always name the same
//! instance — speaking the minimal forever verb set.
//!
//! **One verb per global action, and nothing else** (ADR-0003's
//! 2026-08-26 addendum). A verb exists if and only if it names something
//! a user can bind a key to: no verb reads state, none takes an
//! argument, none composes. This is transport for keys the compositor
//! or the portal presses, not a scripting API — the settings window's
//! status read lives in `shortcuts::state` for exactly that reason.
//!
//! Wire format: one request line (`trigger-down\n`), one reply line
//! (`OK …` / `ERR …`). Boring on purpose: `bindsym` lines shell out to
//! `chibipop ctl`, and a human can drive it with `nc -U`.

use crate::lock::sanitize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The minimal forever verb set: one verb per global action, never a
/// scripting API (ADR-0003 and its 2026-08-26 addendum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Reload,
    TriggerDown,
    TriggerUp,
    Toggle,
    /// Add a card for the lookup on screen - the same action the portal
    /// `anki-add` shortcut performs, and deliberately the same wire
    /// name, so a native bind and a portal press are one thing.
    AnkiAdd,
    /// Grab a region and file it as the mining context for the lookup
    /// on screen (`actions.screenshot`). Native-channel only, for the
    /// same reason as `static-region` below.
    Screenshot,
    /// Pick a region, OCR it, and put the text on the clipboard
    /// (`actions.ocr_clipboard`). Native-channel only, for the same
    /// reason as `static-region` below.
    OcrClipboard,
    /// Draw the box [`chibipop::config::SentenceMode::Static`] reads the
    /// Anki sentence from. Native-channel only: the portal id set stays
    /// at exactly two (ADR-0003's addendum), so this verb *is* the
    /// action's only global channel.
    StaticRegion,
}

pub const VERBS: [Verb; 8] = [
    Verb::Reload,
    Verb::TriggerDown,
    Verb::TriggerUp,
    Verb::Toggle,
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

/// What the received verbs did so far. Placeholder until the core
/// `Controller` lands (ticket 25): a verb updates this and produces a
/// diagnostic line, nothing more.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StubState {
    pub reloads: u32,
    pub trigger_held: bool,
    pub toggled_on: bool,
}

impl StubState {
    /// Apply one verb; the returned line is both the log entry's tail
    /// and the `OK` reply's tail.
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
            // No counter: the daemon's Controller owns whether an add
            // happens at all (no card, empty expression, already
            // added), so a tally here would be a second, wronger
            // answer. The line says what was asked for.
            Verb::AnkiAdd => "card requested for the lookup on screen".to_string(),
            // No counter either, and for a second reason on top of the
            // add's: whether a picture is filed at all depends on the
            // region pick and on whether AnkiConnect can take a card.
            Verb::Screenshot => "picking the mining screenshot's region".to_string(),
            // Same again: the pick, the grab and the recogniser each get
            // to answer nothing, and the clipboard needs a protocol this
            // compositor may not have. The line reports the ask.
            Verb::OcrClipboard => "picking a region to OCR onto the clipboard".to_string(),
            // Same reasoning: the pick itself decides whether a region
            // is set (a cancel, a drag under the threshold, no layer
            // shell), so this line reports the ask, not the answer.
            Verb::StaticRegion => "picking the static sentence region".to_string(),
        }
    }
}

/// The daemon's listening end; unlinks the socket file on drop.
pub struct ControlSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl ControlSocket {
    /// Bind, replacing any stale socket file: the instance lock is the
    /// single-daemon guard, so a file here can only be a previous
    /// daemon's leftover (e.g. after SIGKILL).
    pub fn bind(runtime_dir: &Path, display: &str) -> std::io::Result<ControlSocket> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join(file_name(display));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        // The calloop source polls; accept must never park the pump.
        listener.set_nonblocking(true)?;
        Ok(ControlSocket { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    /// Serve every connection currently queued. Returns one
    /// `(reply_sent, verb_if_valid)` per connection; the caller logs and
    /// applies — this stays free of daemon state.
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
        // Unlike the lock file, a dead socket file is actively harmful:
        // the next daemon must be able to bind, and `ctl` must get
        // "no such file", not a dangling connect.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One request line in, one reply line out.
fn serve_one(stream: UnixStream) -> Option<(String, Option<Verb>)> {
    // Local and short-lived, but never let one wedged client park the
    // daemon: the accepted stream is served blocking with a deadline.
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

/// Client side: send one verb, return the daemon's reply line.
pub fn send(runtime_dir: &Path, display: &str, verb: Verb) -> std::io::Result<String> {
    send_to(&runtime_dir.join(file_name(display)), verb)
}

/// The same exchange against a socket path already in hand.
///
/// The settings process holds the path (it is the ApplyMode: connectable
/// means live-apply, absent means config-only) and must not re-derive it.
pub fn send_to(path: &Path, verb: Verb) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    // Generous on purpose: a verb can land while the daemon is still
    // in startup (the worker pipeline's model load takes seconds on
    // slow hardware); the connect queues and the pump answers as soon
    // as it runs, so a real key press must outwait that window rather
    // than vanish into a client timeout.
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

    /// The literal is the contract: a bind line a user pasted years ago
    /// must keep working, so a rename here is a breaking change and
    /// this assertion is the place it has to be argued.
    #[test]
    fn the_wire_names_are_the_forever_contract() {
        assert_eq!(
            "reload, trigger-down, trigger-up, toggle, anki-add, screenshot, ocr-clipboard, \
             static-region",
            verb_list()
        );
    }

    /// `anki-add` is spelled exactly like the portal shortcut id, so
    /// rung 1 and rung 2 of ADR-0003 name one action, not two.
    #[test]
    fn the_add_verb_and_the_portal_shortcut_id_share_one_name() {
        assert_eq!(crate::shortcuts::ShortcutId::AnkiAdd.as_str(), Verb::AnkiAdd.as_str());
    }

    /// D1, as a property rather than a review habit: the static-region
    /// action has a verb and deliberately *no* portal id, so the consent
    /// dialog did not grow to carry it. The compositor bind is its only
    /// global channel, which is what the settings row's caption says.
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

    /// The same property for OCR-to-clipboard: a verb, no portal id.
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

    /// Compositor-free: a real bind/connect round trip over a temp dir.
    #[test]
    fn a_verb_round_trips_over_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("chibipop_ctl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = ControlSocket::bind(&dir, "test-0").expect("bind");

        let dir2 = dir.clone();
        let client = std::thread::spawn(move || send(&dir2, "test-0", Verb::TriggerDown));

        // Poll the nonblocking listener until the client's request lands.
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

    /// The reply channel rejects garbage without dying.
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
