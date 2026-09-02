//! Test Trigger mode with a real compositor.
//!
//! CI is headless (ARCHITECTURE.md#packaging-and-ci). The test skips without
//! `WAYLAND_DISPLAY`.
//! It also skips when the capture protocol required by the promptless rung is absent.
//! That condition is a compositor state, not a test failure.
//! The daemon can report a third skip condition: a press reads the cursor position, so no
//! cursor rung leaves these verbs without a target.
//! This is the headless-sway case. sway 1.9 advertises no ext-image-copy-capture, runs no
//! portal, and is not Hyprland.
//! `daemon.rs` has tests for that diagnostic.
//!
//! One test drives one daemon with a real screencopy backend, real meikiocr models, and a
//! real SQLite dictionary built from the repository's Yomitan fixtures.
//! It sends the three trigger verbs and reads the daemon log.
//! It synthesizes no input and never touches the seat.
//! Therefore, it does not assert which screen word resolves.
//! That lookup is outside this test. The test checks frozen-hold behavior.
//!
//! The lookup log stays off because the screen belongs to the person at the machine.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// `wlr-screencopy` is rung 1, which this test needs.
/// A portal session would open a consent dialog on the user's screen.
const NEEDED: &str = "zwlr_screencopy_manager_v1";

struct Session {
    dir: PathBuf,
    log: PathBuf,
    daemon: Child,
}

impl Session {
    /// The default session: one term archive, no duplicates.
    fn start(name: &str) -> Session {
        Session::start_with(name, &[("terms.zip", fixture("terms.zip"))])
    }

    /// Create a private XDG tree, link the compositor socket, stock `archives` as
    /// `(name in the library, source)`, build a Dictionary, and start a daemon.
    ///
    /// Build through `Library`, not from a path list. `Library` determines how many
    /// Dictionaries a build makes. Two names for one archive still produce one Dictionary.
    /// A direct path-list build could produce a result that the daemon never uses.
    ///
    /// `name` separates the trees of live sessions.
    /// The tests run in parallel, and each daemon writes its log, socket, and database there.
    fn start_with(name: &str, archives: &[(&str, PathBuf)]) -> Session {
        let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
        let dir = std::env::temp_dir()
            .join(format!("chibipop-trigger-live-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["run/chibipop", "config/chibipop", "data/chibipop/library", "state", "cache"] {
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

        // Build a real Dictionary so the pipeline has all three parts.
        // The test can then assert the log line that names them.
        let library = dir.join("data/chibipop/library");
        for (name, source) in archives {
            std::fs::copy(source, library.join(name)).expect("stocking the library");
        }
        let counts = build_from_library(&library, &dir.join("data/chibipop/chibipop.sqlite"));
        assert!(counts > 0, "the fixture must produce entries");

        // Use Hold-key mode. This test sends the verbs itself.
        // Live mode would run OCR on every cursor move from the person at the machine.
        let mut cfg = chibipop::config::Config::default();
        cfg.trigger.mode = chibipop::config::TriggerMode::HoldKey;
        cfg.save(&dir.join("config/chibipop/chibipop.toml")).expect("writing the config");

        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &dir);
        let daemon = cmd.arg("run").spawn().expect("spawning the chibipop daemon");
        Session { log: dir.join("state/chibipop/chibipop.log"), dir, daemon }
    }

    fn db(&self) -> PathBuf {
        self.dir.join("data/chibipop/chibipop.sqlite")
    }

    fn library(&self) -> PathBuf {
        self.dir.join("data/chibipop/library")
    }

    fn ctl(&self, verb: &str) {
        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &self.dir);
        let out = cmd.args(["ctl", verb]).output().expect("spawning chibipop ctl");
        assert!(out.status.success(), "ctl {verb} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Wait for a log line that contains `needle`, then return it.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(line) = self.log().lines().rev().find(|l| l.contains(needle)) {
                return line.to_string();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("waited 30s for {needle:?}; the log was:\n{}", self.log());
    }

    /// Count the log lines that contain `needle`.
    fn count(&self, needle: &str) -> usize {
        self.log().lines().filter(|l| l.contains(needle)).count()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Send SIGTERM, not kill.
        // The daemon unlinks its socket and drops its lock before exit.
        let _ = Command::new("kill").arg("-TERM").arg(self.daemon.id().to_string()).status();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yomitan").join(name)
}

/// Build the library at `dir` into `out`, as the settings process rebuild thread does.
/// Return the entry count.
fn build_from_library(dir: &Path, out: &Path) -> i64 {
    let lib = chibipop::library::Library::load(dir).expect("reading the library");
    chibipop::dict::build::build(&lib.dict_paths(dir), &lib.freq_paths(dir), out, &|_| {})
        .expect("building the library")
        .entries
}

fn xdg(cmd: &mut Command, dir: &Path) {
    let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
    cmd.env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("WAYLAND_DISPLAY", display);
}

fn skip() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
        return true;
    }
    let probe = Command::new(BIN).arg("probe").output().expect("spawning chibipop probe");
    let report = String::from_utf8_lossy(&probe.stdout).to_string();
    if !report.contains(&format!("{NEEDED} v")) {
        eprintln!("skipping: this compositor advertises no {NEEDED}");
        return true;
    }
    false
}

#[test]
fn the_trigger_verbs_freeze_hold_and_release_a_real_grab() {
    if skip() {
        return;
    }
    let session = Session::start("trigger");

    // Ask the daemon for the cursor gate. Do not derive it from the registry here.
    // Startup logs exactly one `cursor:` line.
    // If it contains `hover unsupported`, a press has no position for a frozen grab.
    // Skip then. Otherwise, the test would wait 30 s for a grab that cannot occur.
    let cursor = session.wait_for("cursor: ");
    if cursor.contains("hover unsupported") {
        eprintln!("skipping: this session gave the cursor ladder no rung - {cursor}");
        return;
    }

    // The worker thread has all three pipeline parts.
    let up = session.wait_for("worker: pipeline up");
    assert!(up.contains("FixtureTerms"), "the built dictionary must be the one it opened: {up}");
    session.wait_for("lookups ready");

    // A press takes one frozen grab of the output under the cursor before any popup exists.
    // The daemon then does the lookup.
    session.ctl("trigger-down");
    let frozen = session.wait_for("trigger: frozen grab of output");
    assert!(frozen.contains("for cursor"), "the grab must name the cursor it froze on: {frozen}");
    assert_eq!(0, session.count("no pipeline"), "the pipeline must have served it");
    assert_eq!(0, session.count("no cursor sample yet"), "the cursor channel must have answered");
    // The screen content does not matter. The lookup itself must succeed.
    assert_eq!(0, session.count("lookup failed"), "log was:\n{}", session.log());

    // Release drops the frozen frame.
    session.ctl("trigger-up");
    session.wait_for("hold released, frozen grab dropped");
    assert_eq!(1, session.count("frozen grab of output"), "one press, one grab");

    // Toggle latches the hold.
    // A stray release must not end it. A second toggle must end it.
    session.ctl("toggle");
    assert_eq!(2, session.count("frozen grab of output"), "toggle-on grabs too");
    session.ctl("trigger-up");
    session.wait_for("a toggle holds the freeze");
    assert_eq!(
        1,
        session.count("hold released"),
        "the release under the latch must not have dropped it"
    );
    session.ctl("toggle");
    assert_eq!(2, session.count("hold released"), "toggle-off ends the hold");

    // A press without a dictionary rebuild must still create only one grab.
    assert_eq!(2, session.count("frozen grab of output"), "no grab without a press");
}

/// This test reproduces the reported failure with a real daemon.
///
/// A user re-imported one Dictionary and rebuilt it. The log contained:
/// ```text
/// worker: pipeline up in 827 ms; 2 dictionary/ies: Jitendex.org [2026-08-11], Jitendex.org [2026-08-11]
/// lookup failed: database disk image is malformed
/// ```
///
/// Two faults appear in the log. The library held two byte-identical copies of one archive,
/// so the build made two Dictionaries from one file.
/// The promote renamed the new database but left the old write-ahead log under its name.
/// The next reader then recovered old pages into the new file.
///
/// Core fixes both faults and tests them separately.
/// This test covers that behavior in a live daemon. Neither fault can be *observed* as the
/// user saw it without a real daemon.
/// The worker's Dictionary and the popup's Media store hold read-only database handles across
/// a promote.
/// One `reload` reopens both handles.
/// The test sends no trigger verb and takes no grab. Daemon startup alone touches the screen.
#[test]
fn a_rebuild_under_a_live_daemon_serves_one_sound_dictionary() {
    if skip() {
        return;
    }
    // A re-import leaves one archive under two names in the library.
    let session = Session::start_with(
        "rebuild",
        &[
            ("[JA-EN] terms (2026-08-11).zip", fixture("terms.zip")),
            ("terms.zip", fixture("terms.zip")),
        ],
    );

    let up = session.wait_for("worker: pipeline up");
    assert!(
        up.contains("1 dictionary/ies"),
        "one archive under two names is one dictionary, not two: {up}"
    );
    session.wait_for("lookups ready");

    // Rebuild while the daemon holds the database open.
    // Use the same core builder and path as the settings process.
    build_from_library(&session.library(), &session.db());
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", session.db().display()));
        assert!(
            !sidecar.exists(),
            "{} must not outlive the promote; a reader would recover it into the new file",
            sidecar.display(),
        );
    }

    // The settings process sends this verb after a successful promote.
    // The daemon must reopen two handles, not one.
    session.ctl("reload");
    session.wait_for("config: reloaded");

    // Open the database that the daemon now serves.
    let reopened = chibipop::lookup::sqlite::SqliteDictionary::open(&session.db())
        .expect("the promoted database must open");
    assert_eq!(1, reopened.dicts().expect("reading the dictionary list").len());
    use chibipop::lookup::model::Dictionary as _;
    // Two rows come from one Dictionary.
    // `terms.zip` carries the kana headword ねこ and the kanji 猫 with the same reading.
    // Each row indexes this surface. A duplicated Dictionary would return four rows.
    assert_eq!(
        2,
        reopened.terms_for("ねこ").expect("the lookup must not fail").len(),
        "the lookup the user could no longer get a popup from",
    );
    assert_eq!(0, session.count("lookup failed"), "log was:\n{}", session.log());
}
