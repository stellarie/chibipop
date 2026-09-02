//! This test checks the KDE and GNOME "user clicked Deny" path end to end with a private portal.
//!
//! The fallback capture rung promises a named channel state and a way back after refusal.
//! It never exits or leaves the daemon without a status for the failed channel.
//! This promise needs a portal that returns a refusal.
//! A developer's portal returns refusal through a consent dialog.
//! `portal_capture_live.rs` does not open that dialog by default, so its real-dialog test needs
//! an opt-in.
//!
//! This file provides a private portal.
//!
//! The test cannot put a dialog on a screen. It starts `dbus-daemon` with a private D-Bus
//! session and a config file with no service directories.
//! No service can activate on that bus.
//! The fake owns `org.freedesktop.portal.Desktop` before the daemon starts.
//! The test gives the daemon child a `DBUS_SESSION_BUS_ADDRESS` value for the private bus.
//! The test process keeps its own environment unchanged.
//! The fake connects with an explicit address.
//! The real session bus, real portal, and tests that run beside this one stay untouched.
//!
//! The fake implements only the `org.freedesktop.portal.ScreenCast` surface that
//! `capture::portal::dbus` uses.
//! That surface includes the `version` and `AvailableCursorModes` properties, plus
//! `CreateSession`, `SelectSources`, and `Start`.
//! Each method answers on the `org.freedesktop.portal.Request` object path that the client
//! predicts from its unique name and `handle_token`.
//! `CreateSession` and `SelectSources` return response code 0.
//! `Start` returns code 1, the wire form of a Deny click, with label "the user cancelled".
//! The fake counts calls.
//! The count proves that the daemon requested the portal again after recovery.
//!
//! This test skips only when `WAYLAND_DISPLAY` is absent or a private bus cannot start.
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

/// The fake owns this name and places every portal interface at this object.
/// Do not import the name. A client rename must not make the fake agree
/// with itself.
const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The interface for deferred portal responses.
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// `Response` code 0 means that the portal completed the request.
/// Code 1 means that the user cancelled it.
/// A Deny button produces only code 1 on the wire, which this test must check.
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// The maximum time that `wait_for` polls the daemon log.
/// The limit allows time for startup to open the OCR models.
const PATIENCE: Duration = Duration::from_secs(60);

/// The number of requests that the daemon sends to the fake.
/// The retry contract uses the `start` count.
/// A reload that only redraws a status row leaves this count unchanged.
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

/// The fake portal returns a denial.
struct ScreenCast {
    calls: Calls,
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl ScreenCast {
    /// The fake reports version 5, like the real portals for this path.
    /// Version 5 supports `persist_mode` and `restore_token`.
    /// The test therefore refuses a persistable grant, not a portal that cannot remember one.
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
        // The client sends `Close` to this object when a granted session ends.
        // Serve that call to avoid `NoSuchObject` for a legal request.
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

    /// This method represents Deny. A real portal shows the dialog at `Start`, so a real
    /// refusal arrives here. An earlier code 1 would test a path that users cannot reach.
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

/// The client can send `Close` to this Request object.
struct RequestObject;

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl RequestObject {
    fn close(&self) {}
}

/// The client can send `Close` to this Session object.
struct SessionObject;

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl SessionObject {
    fn close(&self) {}
}

/// Convert the caller's unique name to an object-path element.
/// The Request documentation and client use this form.
fn sender_of(header: &Header<'_>) -> zbus::fdo::Result<String> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::Failed("the call carried no sender".to_string()))?;
    Ok(sender.as_str().trim_start_matches(':').replace('.', "_"))
}

/// Build the Request object path for `token`.
fn request_path(sender: &str, token: &str) -> String {
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// Get a string token from the portal options by key.
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

/// Send the deferred `Response(u32, a{sv})` signal on the Request object.
/// Emit the signal before the method reply. The client installs its match rule before
/// the call, and a portal that restores a session from a token responds quickly.
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

/// A private D-Bus session bus with no service directories.
/// No real portal can activate on this bus.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    /// Return `None` when `dbus-daemon` cannot provide a bus.
    /// The test treats that result as a skip, not a failure.
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
        // This bus has private clients. It has no state to flush.
        // Kill the daemon rather than signal it with SIGTERM.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The test's scratch state includes a private bus, its fake portal, a private XDG tree
/// with the compositor socket linked, and the real daemon.
struct Session {
    dir: PathBuf,
    log: PathBuf,
    daemon: Child,
    calls: Calls,
    /// Keep the fake's connection for the session lifetime.
    /// If this field drops, the name and objects also drop.
    _portal: zbus::blocking::Connection,
    /// Keep the private bus until the daemon child exits.
    _bus: PrivateBus,
}

impl Session {
    fn start(bus: PrivateBus, dir: PathBuf) -> Session {
        let calls = Calls::default();
        let portal = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .expect("the private bus address must parse")
            .serve_at(PORTAL_PATH, ScreenCast { calls: calls.clone() })
            .expect("serving the fake ScreenCast interface")
            // The fake owns the name before the daemon starts.
            // This order blocks real portal activation on this bus.
            .name(PORTAL_BUS)
            .expect("requesting the portal bus name")
            .build()
            .expect("connecting the fake portal to the private bus");

        // Use hold-key mode. Live mode would run lookups from the user's cursor.
        // This test checks a failed channel, not the hover loop.
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

    /// Wait for a log line that contains `needle`, then return it.
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

    /// Wait until the log and call counts satisfy `done`.
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

    /// Count the log lines that contain `needle`.
    fn count(&self, needle: &str) -> usize {
        self.log().lines().filter(|l| l.contains(needle)).count()
    }

    /// Report whether the daemon child still runs.
    /// A denial must leave this result true.
    fn alive(&mut self) -> bool {
        matches!(self.daemon.try_wait(), Ok(None))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Send SIGTERM, not kill. The daemon unlinks its socket and drops its lock before exit.
        let _ = Command::new("kill").arg("-TERM").arg(self.daemon.id().to_string()).status();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Set a private XDG environment and private bus.
/// The daemon must not read the developer's config or rotate the real restore token.
fn xdg(command: &mut Command, dir: &Path, bus: &str) {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    command
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("WAYLAND_DISPLAY", display)
        // The first hook forces rung 2 because this box advertises screencopy and would
        // otherwise never reach the portal. The second hook directs the daemon to the fake.
        .env("CHIBIPOP_CAPTURE_BACKEND", "portal")
        .env("DBUS_SESSION_BUS_ADDRESS", bus);
}

/// Create the scratch tree and link the compositor socket.
/// This lets the daemon reach the real display with a private `XDG_RUNTIME_DIR`.
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

/// Keep this test as one test, not two.
/// The retry contract concerns the same daemon that received the refusal.
/// That daemon stays alive, and `reload` sends it to the portal again.
/// Two tests would need two daemons, two buses, and a second startup consent instead of the
/// retry that this test must prove.
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

    // The first handshake uses the fake.
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
        "the diagnostic must name the way back: {failed}"
    );

    // The Capture channel row reports this detail after it goes down.
    // The tray shows this row, so the row must name the state.
    // The test checks a substring of one row because more channels can appear.
    let row = session.wait_for("channel: Capture: ");
    assert!(
        row.contains("screen-capture permission denied") && row.contains("chibipop ctl reload"),
        "the Capture row must name the denial and the retry: {row}"
    );

    // Startup finishes, and the process remains alive.
    // Refused permission disables one channel, not the installation.
    let ready = session.wait_for("ready: pump running");
    assert!(ready.contains("capture portal"), "the portal rung is still the selected one: {ready}");
    assert!(session.alive(), "a denial must never exit the daemon");

    // The refusal persists no grant.
    // No token must remain, or the next launch could skip the required dialog.
    let token = session.dir.join("state/chibipop/portal-restore-token");
    assert!(!token.exists(), "a refusal must leave no restore token at {}", token.display());

    // -- the retry --

    let before = session.calls.counts();
    assert_eq!(1, session.count("capture: portal consent failed"), "exactly one refusal so far");
    session.ctl("reload");

    // `reload` must send a new request over the bus.
    // A log line alone could pass if the daemon only redraws a row.
    // The fake call count must increase too.
    session.wait_until("the reload to re-request consent", |log, calls| {
        log.contains("capture: retrying the portal consent") && calls.2 > before.2
    });
    let (created, selected, started) = session.calls.counts();
    assert_eq!(
        (before.0 + 1, before.1 + 1, before.2 + 1),
        (created, selected, started),
        "the retry must be a whole fresh handshake, once"
    );

    // The second refusal must match the first.
    // The daemon must remain alive and ready for another retry.
    session.wait_until("the second refusal", |log, _| {
        log.matches("capture: portal consent failed").count() >= 2
    });
    let again = session.wait_for("capture: portal consent failed");
    assert!(again.contains("screen-capture permission denied"), "{again}");
    assert!(session.alive(), "a second denial must not exit the daemon either");
    assert!(!token.exists(), "and still no restore token");
}
