//! The KDE/GNOME "user clicked Deny" path, end to end, against a portal
//! that only exists inside this test.
//!
//! ADR-0002 makes one promise about its fallback capture rung that no
//! other test in this tree can check: a refusal is a *named channel
//! state with a way back*, never an exit and never a silent
//! half-working daemon. Checking it needs a portal that says no, and
//! the only portal on a developer's machine says no by putting a
//! consent dialog on their screen - which is exactly what
//! `portal_capture_live.rs` refuses to do, and why its one real-dialog
//! test is opt-in.
//!
//! So this file brings its own portal.
//!
//! **Nothing here can ever put a dialog on anyone's screen.** The fake
//! runs on a *private* D-Bus session started by this test
//! (`dbus-daemon` with a config file carrying no service directories at
//! all, so nothing on that bus can be activated even in principle), it
//! owns `org.freedesktop.portal.Desktop` on that bus before the daemon
//! is spawned, and the daemon child is handed
//! `DBUS_SESSION_BUS_ADDRESS` pointing at it. The test process never
//! mutates its own environment - the fake is connected to the private
//! bus by explicit address - so the real session bus, the real portal
//! and every test running beside this one are untouched.
//!
//! The fake speaks only as much `org.freedesktop.portal.ScreenCast` as
//! `capture::portal::dbus` actually uses: the `version` and
//! `AvailableCursorModes` properties, `CreateSession`, `SelectSources`
//! and `Start`, each answering at the `org.freedesktop.portal.Request`
//! object path the client predicts from our unique name and its
//! `handle_token`. `CreateSession` and `SelectSources` answer response
//! code 0; `Start` answers code 1, "the user cancelled" - the wire
//! shape of somebody clicking Deny. It counts its calls, which is what
//! turns "the daemon recovered" into "the daemon *asked again*".
//!
//! Skips, and only for honest reasons: no `WAYLAND_DISPLAY` (CI is
//! headless - ADR-0007) and no way to start a private bus.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zbus::message::Header;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, ObjectServer};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The name the fake owns, and the object every portal interface lives
/// on. Spelled here rather than imported so a rename in the client
/// cannot silently make the fake agree with itself.
const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// Where every portal method's deferred answer arrives.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// `Response` code 0: carried out. Code 1: the user cancelled - the
/// only thing a Deny button produces on the wire, and the whole point
/// of this file.
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// How long a `wait_for` may poll the daemon's log. Generous: the
/// startup this waits on opens the OCR models.
const PATIENCE: Duration = Duration::from_secs(60);

/// How many times the fake was asked each question. The whole retry
/// contract rests on `start`: a reload that only re-rendered a status
/// row would leave it where it was.
#[derive(Clone, Default)]
struct Calls {
    create_session: Arc<AtomicUsize>,
    select_sources: Arc<AtomicUsize>,
    start: Arc<AtomicUsize>,
}

impl Calls {
    fn counts(&self) -> (usize, usize, usize) {
        (
            self.create_session.load(Ordering::SeqCst),
            self.select_sources.load(Ordering::SeqCst),
            self.start.load(Ordering::SeqCst),
        )
    }
}

/// The denying portal.
struct ScreenCast {
    calls: Calls,
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl ScreenCast {
    /// v5, like the portals on the desktops this path exists for: new
    /// enough that the client sends `persist_mode` and a
    /// `restore_token`, so the refusal being tested is a refusal of a
    /// *persistable* grant rather than of a portal that could never
    /// remember one.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        5
    }

    /// HIDDEN | EMBEDDED | METADATA, as KDE and GNOME advertise it.
    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        7
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        self.calls.create_session.fetch_add(1, Ordering::SeqCst);
        let sender = sender_of(&header)?;
        let request = request_path(&sender, &token(&options, "handle_token")?);
        let session =
            format!("{PORTAL_PATH}/session/{sender}/{}", token(&options, "session_handle_token")?);
        // The client `Close`s this on the way out of a *granted*
        // session; serving it costs one line and keeps the fake from
        // answering NoSuchObject to a legal call.
        serve(server, &session, SessionObject).await?;
        serve(server, &request, RequestObject).await?;

        let mut results: HashMap<&str, Value<'_>> = HashMap::new();
        results.insert("session_handle", Value::from(session.as_str()));
        respond(conn, &request, RESPONSE_SUCCESS, results).await?;
        path_of(&request)
    }

    async fn select_sources(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        self.calls.select_sources.fetch_add(1, Ordering::SeqCst);
        let request = request_path(&sender_of(&header)?, &token(&options, "handle_token")?);
        serve(server, &request, RequestObject).await?;
        respond(conn, &request, RESPONSE_SUCCESS, HashMap::new()).await?;
        path_of(&request)
    }

    /// The Deny. `Start` is where a real portal draws the dialog, so it
    /// is where a real refusal lands, and answering code 1 anywhere
    /// earlier would test a path no user can reach.
    async fn start(
        &self,
        _session: OwnedObjectPath,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        self.calls.start.fetch_add(1, Ordering::SeqCst);
        let request = request_path(&sender_of(&header)?, &token(&options, "handle_token")?);
        serve(server, &request, RequestObject).await?;
        respond(conn, &request, RESPONSE_CANCELLED, HashMap::new()).await?;
        path_of(&request)
    }
}

/// A Request object, for the `Close` a client is entitled to send.
struct RequestObject;

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl RequestObject {
    fn close(&self) {}
}

/// A Session object, same reason.
struct SessionObject;

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl SessionObject {
    fn close(&self) {}
}

/// The caller's unique name as an object-path element, exactly as the
/// Request documentation specifies and as the client predicts it.
fn sender_of(header: &Header<'_>) -> zbus::fdo::Result<String> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::Failed("the call carried no sender".to_string()))?;
    Ok(sender.as_str().trim_start_matches(':').replace('.', "_"))
}

/// Where the portal must put the Request object for `token`.
fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// One `s` option, by key.
fn token(options: &HashMap<String, OwnedValue>, key: &str) -> zbus::fdo::Result<String> {
    match options.get(key).map(|value| &**value) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(zbus::fdo::Error::InvalidArgs(format!("no {key} in the options"))),
    }
}

fn path_of(path: &str) -> zbus::fdo::Result<OwnedObjectPath> {
    ObjectPath::try_from(path.to_string())
        .map(Into::into)
        .map_err(|e| zbus::fdo::Error::Failed(format!("{path} is not an object path: {e}")))
}

async fn serve<I>(server: &ObjectServer, path: &str, iface: I) -> zbus::fdo::Result<()>
where
    I: zbus::object_server::Interface,
{
    server
        .at(path_of(path)?, iface)
        .await
        .map(|_| ())
        .map_err(|e| zbus::fdo::Error::Failed(format!("serving {path}: {e}")))
}

/// The deferred half of a portal method: `Response(u32, a{sv})` on the
/// Request object. Emitted before the method reply on purpose - the
/// client registers its match rule *before* it calls, and a portal
/// restoring a session from a token really does answer this fast.
async fn respond(
    conn: &Connection,
    request: &str,
    code: u32,
    results: HashMap<&str, Value<'_>>,
) -> zbus::fdo::Result<()> {
    conn.emit_signal(None::<&str>, request, REQUEST_INTERFACE, "Response", &(code, results))
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("emitting Response: {e}")))
}

/// A D-Bus session bus of our own, with no service directories: nothing
/// on it can be activated, so no real portal can ever appear on it.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    /// `None` when `dbus-daemon` cannot give us a bus - a skip, not a
    /// failure.
    fn start(dir: &Path) -> Option<PrivateBus> {
        let config = dir.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig>\n\
                 <type>session</type>\n\
                 <listen>unix:tmpdir={}</listen>\n\
                 <policy context=\"default\">\n\
                 <allow send_destination=\"*\" eavesdrop=\"true\"/>\n\
                 <allow eavesdrop=\"true\"/>\n\
                 <allow own=\"*\"/>\n\
                 </policy>\n\
                 </busconfig>\n",
                dir.display()
            ),
        )
        .expect("writing the private bus config");

        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        let read = daemon
            .stdout
            .take()
            .map(|out| BufReader::new(out).read_line(&mut address))
            .and_then(Result::ok);
        match read {
            Some(n) if n > 0 => Some(PrivateBus { daemon, address: address.trim().to_string() }),
            _ => {
                let _ = daemon.kill();
                let _ = daemon.wait();
                None
            }
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // Our own bus, with our own clients: nothing here has state
        // worth flushing, so this is a kill rather than a SIGTERM.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The scratch world: a private bus, the fake portal on it, a private
/// XDG tree with the compositor's socket linked in, and the real daemon
/// on top of all of it.
struct Session {
    dir: PathBuf,
    log: PathBuf,
    daemon: Child,
    calls: Calls,
    /// The fake's connection: dropping it drops the name and the
    /// objects, so it lives exactly as long as the session.
    _portal: zbus::blocking::Connection,
    /// Dropped last of all, after the daemon child is reaped.
    _bus: PrivateBus,
}

impl Session {
    fn start(bus: PrivateBus, dir: PathBuf) -> Session {
        let calls = Calls::default();
        let portal = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .expect("the private bus address must parse")
            .serve_at(PORTAL_PATH, ScreenCast { calls: calls.clone() })
            .expect("serving the fake ScreenCast interface")
            // Owned before the daemon starts, which is also what makes
            // activation of the real portal impossible on this bus.
            .name(PORTAL_BUS)
            .expect("requesting the portal bus name")
            .build()
            .expect("connecting the fake portal to the private bus");

        // Hold-key mode: live mode would drive lookups off whoever is
        // at the machine, and this test is about a channel that is
        // down, not about hovering.
        let mut config = chibipop::config::Config::default();
        config.trigger.mode = chibipop::config::TriggerMode::HoldKey;
        config.save(&dir.join("config/chibipop/chibipop.toml")).expect("writing the config");

        let mut command = Command::new(BIN);
        xdg(&mut command, &dir, &bus.address);
        let daemon = command.arg("run").spawn().expect("spawning the chibipop daemon");
        Session {
            log: dir.join("state/chibipop/chibipop.log"),
            dir,
            daemon,
            calls,
            _portal: portal,
            _bus: bus,
        }
    }

    fn ctl(&self, verb: &str) {
        let mut command = Command::new(BIN);
        xdg(&mut command, &self.dir, &self._bus.address);
        let out = command.args(["ctl", verb]).output().expect("spawning chibipop ctl");
        assert!(
            out.status.success(),
            "ctl {verb} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Wait for a line containing `needle`, and answer it.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Some(line) = self.log().lines().rev().find(|l| l.contains(needle)) {
                return line.to_string();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("waited {PATIENCE:?} for {needle:?}; the log was:\n{}", self.log());
    }

    /// Wait until `done` is satisfied by the log and the call counts.
    fn wait_until(&self, what: &str, done: impl Fn(&str, (usize, usize, usize)) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let log = self.log();
            if done(&log, self.calls.counts()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "waited {PATIENCE:?} for {what}; calls were {:?} and the log was:\n{}",
            self.calls.counts(),
            self.log()
        );
    }

    /// How many lines carry `needle` right now.
    fn count(&self, needle: &str) -> usize {
        self.log().lines().filter(|l| l.contains(needle)).count()
    }

    /// Whether the daemon child is still running. A denial must never
    /// answer anything but `true`.
    fn alive(&mut self) -> bool {
        matches!(self.daemon.try_wait(), Ok(None))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SIGTERM, not a kill: the daemon unlinks its socket and drops
        // its lock on the way out.
        let _ = Command::new("kill").arg("-TERM").arg(self.daemon.id().to_string()).status();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A private XDG world plus the private bus, so nothing here reads the
/// developer's config or rotates their real restore token.
fn xdg(command: &mut Command, dir: &Path, bus: &str) {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    command
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("WAYLAND_DISPLAY", display)
        // The two hooks this whole file rests on: rung 2 forced (this
        // box advertises screencopy and would never reach the portal on
        // its own), talking to the fake and to nothing else.
        .env("CHIBIPOP_CAPTURE_BACKEND", "portal")
        .env("DBUS_SESSION_BUS_ADDRESS", bus);
}

/// The scratch tree, with the compositor's socket linked in so the
/// daemon can reach the real display from a private `XDG_RUNTIME_DIR`.
fn scratch() -> PathBuf {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    let dir = std::env::temp_dir().join(format!("chibipop-portal-denial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["run/chibipop", "config/chibipop", "data/chibipop", "state", "cache"] {
        std::fs::create_dir_all(dir.join(sub)).expect("creating the scratch tree");
    }
    if !display.starts_with('/') {
        let runtime = std::env::var("XDG_RUNTIME_DIR").expect("a session runtime dir");
        std::os::unix::fs::symlink(
            PathBuf::from(runtime).join(&display),
            dir.join("run").join(&display),
        )
        .expect("linking the compositor socket into the scratch tree");
    }
    dir
}

/// One test, not two, and deliberately: the retry contract is a claim
/// about *the same daemon* that was refused - that it never exited and
/// that a `reload` sends it back to the portal - so splitting it would
/// mean two daemons, two buses, and a second startup consent standing
/// in for the retry it is supposed to prove.
#[test]
fn a_denied_capture_portal_stays_up_named_and_recovers_on_reload() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return;
    }
    let dir = scratch();
    let Some(bus) = PrivateBus::start(&dir) else {
        eprintln!("skipping: dbus-daemon could not start a private session bus");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let mut session = Session::start(bus, dir);

    // -- the refusal --

    // The handshake really ran against the fake, once.
    let asked = session.wait_for("portal: requesting screen capture consent");
    assert!(asked.contains("a dialog will appear"), "the first launch has no token: {asked}");
    session.wait_until("the first consent round", |_, calls| calls == (1, 1, 1));

    let failed = session.wait_for("capture: portal consent failed");
    assert!(
        failed.contains("screen-capture permission denied"),
        "a Deny must read as a denial, not as an absence or a timeout: {failed}"
    );
    assert!(
        failed.contains("chibipop ctl reload"),
        "the diagnostic must name the way back (ADR-0002): {failed}"
    );

    // The Capture channel row went down carrying that same detail: this
    // is what the tray shows and what ADR-0006 calls a named state.
    // Asserted as a substring of one row, not against the row list -
    // the set of channels grows.
    let row = session.wait_for("channel: Capture: ");
    assert!(
        row.contains("screen-capture permission denied") && row.contains("chibipop ctl reload"),
        "the Capture row must name the denial and the retry: {row}"
    );

    // Startup finished anyway, and the process is still there. A
    // refused permission is a channel that is down, not an install that
    // is broken.
    let ready = session.wait_for("ready: pump running");
    assert!(ready.contains("capture portal"), "the portal rung is still the selected one: {ready}");
    assert!(session.alive(), "a denial must never exit the daemon");

    // Nothing was persisted: there is no grant to remember, and a token
    // file left behind would make the next launch silently skip the
    // dialog it needs.
    let token = session.dir.join("state/chibipop/portal-restore-token");
    assert!(!token.exists(), "a refusal must leave no restore token at {}", token.display());

    // -- the retry --

    let before = session.calls.counts();
    assert_eq!(1, session.count("capture: portal consent failed"), "exactly one refusal so far");
    session.ctl("reload");

    // The claim under test: `reload` goes back out on the bus. The log
    // line alone would pass on a daemon that only re-rendered a row, so
    // the fake's own counter has to move with it.
    session.wait_until("the reload to re-request consent", |log, calls| {
        log.contains("capture: retrying the portal consent") && calls.2 > before.2
    });
    let (created, selected, started) = session.calls.counts();
    assert_eq!(
        (before.0 + 1, before.1 + 1, before.2 + 1),
        (created, selected, started),
        "the retry must be a whole fresh handshake, once"
    );

    // And the second answer is reported exactly like the first, on a
    // daemon that is still up and still retryable.
    session.wait_until("the second refusal", |log, _| {
        log.matches("capture: portal consent failed").count() >= 2
    });
    let again = session.wait_for("capture: portal consent failed");
    assert!(again.contains("screen-capture permission denied"), "{again}");
    assert!(session.alive(), "a second denial must not exit the daemon either");
    assert!(!token.exists(), "and still no restore token");
}
