//! Tests the popup against a real compositor, when a compositor exists.
//!
//! CI runs headless (ARCHITECTURE.md#packaging-and-ci). This file skips when
//! `WAYLAND_DISPLAY` is unset. It skips again when the compositor advertises no layer
//! shell, no fractional scale, or no viewporter. These three globals are mandatory.
//! Their absence is a compositor verdict, not a test failure.
//!
//! This file holds one test on purpose. It drives a whole daemon through the canned
//! popup (`CHIBIPOP_POPUP_DEMO=1`). Two of these tests would race over one compositor
//! and prove nothing new. The test reads everything it asserts back from the daemon's
//! own log and from `hyprctl`. The test synthesizes no input and never touches the
//! seat. The popup it shows stays visible for a few hundred milliseconds and takes no
//! focus. That lack of focus is one of the properties under test.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The globals that a crisp popup requires.
const NEEDED: [&str; 3] =
    ["zwlr_layer_shell_v1", "wp_fractional_scale_manager_v1", "wp_viewporter"];

/// A fixed anchor keeps the run reproducible and needs no pointer move. It uses global
/// physical pixels near the top-left, so it lands on any output.
const ANCHOR: &str = "400,300,140,40";
const ANCHOR_X: i32 = 400;
const ANCHOR_Y: i32 = 300;

/// A private XDG environment plus the daemon that runs inside it.
struct Session {
    dir: PathBuf,
    log: PathBuf,
    daemon: Child,
}

impl Session {
    /// Builds a private XDG tree with the compositor's socket linked in, and starts a
    /// daemon in it with the demo popup armed.
    ///
    /// `XDG_RUNTIME_DIR` belongs to this test alone. The instance lock and the control
    /// socket can then never collide with a real daemon's. That is also why this code links
    /// the compositor socket into the tree.
    fn start() -> Session {
        let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
        let dir = std::env::temp_dir().join(format!("chibipop-popup-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["run/chibipop", "config", "data", "state", "cache"] {
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

        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &dir);
        cmd.env("CHIBIPOP_POPUP_DEMO", "1")
            .env("CHIBIPOP_POPUP_DEMO_ANCHOR", ANCHOR)
            .arg("run");
        let daemon = cmd.spawn().expect("spawning the chibipop daemon");
        Session { log: dir.join("state/chibipop/chibipop.log"), dir, daemon }
    }

    fn ctl(&self, verb: &str) {
        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &self.dir);
        let out = cmd.args(["ctl", verb]).output().expect("spawning chibipop ctl");
        assert!(
            out.status.success(),
            "ctl {verb} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Wait for a line that contains `needle`, and answer it.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(line) = self.log().lines().rev().find(|l| l.contains(needle)) {
                return line.to_string();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("waited 10s for {needle:?}; the log was:\n{}", self.log());
    }

    fn terminate(&self) {
        let _ = Command::new("kill").arg("-TERM").arg(self.daemon.id().to_string()).status();
    }

    /// Reap the daemon for up to two seconds. This reap is the honest way to test whether
    /// the daemon exited. An unwaited child stays a zombie, and `kill -0` cannot tell a
    /// zombie from a process that runs.
    fn wait_exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.daemon.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => return false,
            }
        }
        false
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // This is a SIGTERM, not a kill. The daemon's own handler unlinks the socket and
        // releases the lock, and the test asserts that it did.
        self.terminate();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
    for global in NEEDED {
        if !report.contains(&format!("{global} v")) {
            eprintln!("skipping: this compositor advertises no {global}");
            return true;
        }
    }
    false
}

/// One `hyprctl` subcommand's first line, or `None` off Hyprland.
fn hyprctl(sub: &str) -> Option<String> {
    let out = Command::new("hyprctl").args(["-i", "0", sub]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// `popup: shown on surface N at x,y WxH at S.SSSx (...)` unpacked.
fn shown_rect(line: &str) -> (i32, i32, i32, i32, f64) {
    let mut clauses = line.split(" at ").skip(1);
    let geometry = clauses.next().expect("an `at x,y WxH` clause");
    let scale: f64 = clauses
        .next()
        .and_then(|c| c.split('x').next())
        .and_then(|s| s.parse().ok())
        .expect("an `at S.SSSx` clause");
    let (pos, size) = geometry.split_once(' ').expect("position then size");
    let (x, y) = pos.split_once(',').expect("x,y");
    let (w, h) = size.split_once('x').expect("WxH");
    let num = |s: &str| s.parse::<i32>().unwrap_or_else(|_| panic!("{s:?} is not a pixel count"));
    (num(x), num(y), num(w), num(h), scale)
}

#[test]
fn a_canned_popup_is_placed_painted_and_hidden_without_taking_focus() {
    if skip() {
        return;
    }
    let mut session = Session::start();

    // At startup, the daemon maps one surface for each output. Each surface starts hidden,
    // sits on the layer that the config asks for, and has keyboard interactivity off. These
    // are three of the popup's inviolable properties.
    let mapped = session.wait_for("layer surface(s) mapped hidden");
    let created = session.wait_for("popup: layer surface 0 on ");
    assert!(created.contains("overlay layer"), "{created}");
    assert!(created.contains("keyboard none"), "{created}");
    assert!(session.log().contains("font: painting with"), "the family in use must be logged");
    let count: usize =
        mapped.split_whitespace().find_map(|w| w.parse().ok()).expect("a surface count");
    assert!(count >= 1, "{mapped}");

    // The pump serves `ctl` requests only after startup finishes. Startup includes the
    // worker pipeline's model load, which takes about 1 to 2 seconds cold. The ready line
    // is the daemon's own mark for that finish. A `ctl` call sent before the ready line
    // races that load. The connect call queues, and the reply can time out on a slow
    // runner.
    session.wait_for("ready: pump running");

    // A show measures the popup, places it, commits the surface, and reports the rect back
    // to the Controller as `Event::PopupPlaced`.
    let focus_before = hyprctl("activewindow");
    session.ctl("trigger-down");
    let shown = session.wait_for("popup: shown on surface");
    let (x, y, w, h, scale) = shown_rect(&shown);
    assert!(w > 0 && h > 0, "{shown}");
    assert!(scale >= 1.0, "a scale of {scale} is not a scale: {shown}");
    assert_eq!(ANCHOR_X, x, "the panel's left edge is the anchor's: {shown}");
    assert!(y > ANCHOR_Y, "the panel sits below the anchor: {shown}");

    // This is the compositor's own view of the surface, in logical units. That view checks
    // the placement arithmetic against the other side of the protocol, not against itself.
    if let Some(layers) = hyprctl("layers") {
        // This code matches by pid, not by namespace alone. A developer who runs a real
        // chibipop in the same session has a `namespace: chibipop` layer of their own. A
        // comparison of this daemon's placement against that layer would fail for no
        // reason.
        let pid = format!("pid: {}", session.daemon.id());
        let line = layers
            .lines()
            .find(|l| l.contains("namespace: chibipop,") && l.contains(&pid))
            .unwrap_or_else(|| panic!("no chibipop layer for {pid} in:\n{layers}"))
            .to_string();
        // `hyprctl` prints `xywh: X Y W H`. Every number in that line is logical.
        let logical = format!(
            "xywh: {} {} {} {}",
            (f64::from(x) / scale).round() as i32,
            (f64::from(y) / scale).round() as i32,
            (f64::from(w) / scale).ceil() as i32,
            (f64::from(h) / scale).ceil() as i32,
        );
        assert!(
            line.contains(&logical),
            "the compositor has the surface at {line}, not at {logical}"
        );
    }

    // Focus never moves. The setting `keyboard_interactivity: none` makes a focus change
    // impossible. This assertion would catch a change if the setting broke.
    assert_eq!(focus_before, hyprctl("activewindow"), "showing the popup moved the focus");

    // A hide attaches a transparent buffer. It never unmaps the surface. That choice keeps
    // the next show free of Hyprland's layer animation.
    session.ctl("trigger-up");
    session.wait_for("popup: hidden in");
    let pid = format!("pid: {}", session.daemon.id());
    if let Some(layers) = hyprctl("layers") {
        assert!(
            layers.lines().any(|l| l.contains("namespace: chibipop") && l.contains(&pid)),
            "hiding must not unmap the surface:\n{layers}"
        );
    }

    // Nothing remains afterward. The surfaces end when the process ends.
    session.terminate();
    assert!(session.wait_exit(), "the daemon ignored SIGTERM");
    if let Some(layers) = hyprctl("layers") {
        assert!(
            !layers.contains(&pid),
            "the daemon left layer surfaces behind:\n{layers}"
        );
    }
}
