//! Trigger mode against a real compositor, when there is one.
//!
//! CI is headless (ADR-0007), so this skips without `WAYLAND_DISPLAY` and
//! skips again where the capture protocol ADR-0002's promptless rung needs
//! is not advertised - that is a compositor verdict, not a test failure.
//! A third verdict skips it too, and the daemon is the one who gives it:
//! a press looks up what is under the cursor, so a session where
//! ADR-0003's cursor ladder found no rung has nothing for these verbs to
//! act on. That is the ticket-48 headless-sway case (sway 1.9 advertises
//! no ext-image-copy-capture, runs no portal and is not Hyprland); the
//! diagnostic it prints instead has its own tests in `daemon.rs`.
//!
//! One test, deliberately: it drives a whole daemon - real screencopy
//! backend, real meikiocr models, real SQLite dictionary built here from
//! the repo's Yomitan fixtures - through the three trigger verbs and reads
//! everything back out of the daemon's own log. It synthesizes no input
//! and never touches the seat, so it cannot know what is on the screen and
//! asserts nothing about the words it reads: which word resolves is the
//! smoke's business (ticket 35's Comments), and the frozen-hold mechanism
//! is this one's.
//!
//! The lookup log stays off here for the same reason: the screen belongs
//! to whoever is at the machine.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// wlr-screencopy: rung 1, and the one this test wants - a portal session
/// would open a consent dialog in someone's face.
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

    /// A private XDG tree with the compositor's socket linked in, a library
    /// stocked with `archives` as `(name in the library, source)`, a
    /// dictionary built out of that library, and a daemon started on top.
    ///
    /// Built through `Library` rather than from a path list, because the
    /// library is what decides how many dictionaries a build makes - two
    /// names for one archive are one dictionary - and a test that skipped it
    /// would build something the daemon never would.
    ///
    /// `name` keeps two live sessions out of each other's tree: the tests
    /// here run in parallel, and each daemon writes its own log, socket and
    /// database under this directory.
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

        // A real dictionary, so the pipeline has all three parts and the
        // log line naming them is worth asserting.
        let library = dir.join("data/chibipop/library");
        for (name, source) in archives {
            std::fs::copy(source, library.join(name)).expect("stocking the library");
        }
        let counts = build_from_library(&library, &dir.join("data/chibipop/chibipop.sqlite"));
        assert!(counts > 0, "the fixture must produce entries");

        // Hold-key mode: this test presses the verbs itself, and live
        // mode would OCR on every cursor move of whoever is at the
        // machine.
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

    /// Wait for a line containing `needle`, and answer it.
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

    /// How many lines carry `needle` right now.
    fn count(&self, needle: &str) -> usize {
        self.log().lines().filter(|l| l.contains(needle)).count()
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

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yomitan").join(name)
}

/// Builds the library at `dir` into `out`, exactly as the settings
/// process's rebuild thread does, and answers the entry count.
fn build_from_library(dir: &Path, out: &Path) -> i64 {
    let lib = chibipop::library::Library::load(dir).expect("reading the library");
    chibipop::dict::build::build(&lib.term_paths(dir), &lib.freq_paths(dir), out, &|_| {})
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

    // The cursor gate, asked of the daemon rather than re-derived from
    // the registry here: it logs exactly one `cursor:` line at startup,
    // and `hover unsupported` in it means a press has no position to
    // freeze on. Skipping is the honest answer; the alternative is a
    // 30 s wait for a grab that cannot happen.
    let cursor = session.wait_for("cursor: ");
    if cursor.contains("hover unsupported") {
        eprintln!("skipping: this session gave the cursor ladder no rung - {cursor}");
        return;
    }

    // All three parts of the pipeline, on the worker thread.
    let up = session.wait_for("worker: pipeline up");
    assert!(up.contains("FixtureTerms"), "the built dictionary must be the one it opened: {up}");
    session.wait_for("lookups ready");

    // A press: one frozen grab of the output under the cursor, taken
    // before any popup exists, and the lookup that follows it.
    session.ctl("trigger-down");
    let frozen = session.wait_for("trigger: frozen grab of output");
    assert!(frozen.contains("for cursor"), "the grab must name the cursor it froze on: {frozen}");
    assert_eq!(0, session.count("no pipeline"), "the pipeline must have served it");
    assert_eq!(0, session.count("no cursor sample yet"), "the cursor channel must have answered");
    // Whatever is on this screen, the read itself must not have failed.
    assert_eq!(0, session.count("lookup failed"), "log was:\n{}", session.log());

    // Release drops the frame.
    session.ctl("trigger-up");
    session.wait_for("hold released, frozen grab dropped");
    assert_eq!(1, session.count("frozen grab of output"), "one press, one grab");

    // Toggle latches: a stray release must not end it, a second toggle
    // must (ADR-0010).
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

    // And a press with no dictionary rebuild in sight still only ever
    // grabbed once per press.
    assert_eq!(2, session.count("frozen grab of output"), "no grab without a press");
}

/// The reported failure against a real daemon.
///
/// A user re-imported their one dictionary and rebuilt, and their log read:
///
/// ```text
/// worker: pipeline up in 827 ms; 2 dictionary/ies: Jitendex.org [2026-08-11], Jitendex.org [2026-08-11]
/// lookup failed: database disk image is malformed
/// ```
///
/// Two faults in two lines. The library held two byte-identical copies of
/// one archive, so the build made two dictionaries out of one file; and the
/// promote renamed the new database in while leaving the old one's
/// write-ahead log under its name, so the next reader recovered the old
/// pages into the new file.
///
/// Both are fixed in core and have their own tests there. This one exists
/// because neither can be *observed* the way the user met them without a
/// real daemon: two live read-only handles onto the database (the worker's
/// dictionary and the popup's media store), held across a promote, then
/// reopened by one `reload`. It presses no trigger verb and takes no grab
/// of its own - the daemon's startup is the only thing that touches the
/// screen.
#[test]
fn a_rebuild_under_a_live_daemon_serves_one_sound_dictionary() {
    if skip() {
        return;
    }
    // The library a re-import leaves behind: one archive, twice, under two
    // names.
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

    // The rebuild, run while the daemon holds the database open - the same
    // core builder on the same path the settings process uses.
    build_from_library(&session.library(), &session.db());
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", session.db().display()));
        assert!(
            !sidecar.exists(),
            "{} must not outlive the promote; a reader would recover it into the new file",
            sidecar.display(),
        );
    }

    // The one verb the settings process sends after a successful promote.
    // It has two handles to reopen in there, not one.
    session.ctl("reload");
    session.wait_for("config: reloaded");

    // The database the daemon is now serving, read cold.
    let reopened = chibipop::lookup::sqlite::SqliteDictionary::open(&session.db())
        .expect("the promoted database must open");
    assert_eq!(1, reopened.dicts().expect("reading the dictionary list").len());
    use chibipop::lookup::model::Dictionary as _;
    // Two rows, and both from one dictionary: `terms.zip` carries the kana
    // headword ねこ and the kanji 猫 that reads the same, and each indexes
    // this surface. A duplicated dictionary would have answered four.
    assert_eq!(
        2,
        reopened.terms_for("ねこ").expect("the lookup must not fail").len(),
        "the lookup the user could no longer get a popup from",
    );
    assert_eq!(0, session.count("lookup failed"), "log was:\n{}", session.log());
}
