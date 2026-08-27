//! The region selector and the outline overlay against a real
//! compositor, when there is one.
//!
//! Sibling of `popup_live.rs` and the same shape: CI is headless
//! (ADR-0007), so this skips without `WAYLAND_DISPLAY` - and skips again
//! on a compositor advertising no layer shell, fractional scale or
//! viewporter, because those are the globals ADR-0004 makes mandatory
//! and their absence is a compositor verdict, not a test failure.
//!
//! It drives a whole daemon through `CHIBIPOP_SURFACE_PROBE=1`, which
//! feeds two known scan rects through the shipped
//! `Command::ShowScanOverlay` consumer, runs one region pick, and takes
//! both down. Everything asserted is read back out of the daemon's
//! own log and out of `hyprctl`; it synthesizes no input and never
//! touches the seat.
//!
//! **What this cannot prove.** A drag needs a press, a motion and a
//! release from the seat, and this crate's rule is that no test
//! synthesizes seat input (`popup_live.rs`: "it synthesizes no input and
//! never touches the seat" - the pointer script exists precisely so the
//! popup's handlers can be driven without one). There is no equivalent
//! hook here, because a script would be driving the drag arithmetic
//! rather than the compositor. So the drag itself is pinned by the unit
//! tests in `select.rs` (`Drag`/`Ask`, in physical pixels, negative
//! directions, the threshold, both cancels), and what is pinned *here*
//! is everything a compositor has to agree to: that the surfaces map on
//! the right layer with the right keyboard interactivity, that their
//! geometry converts through the output's fractional scale exactly as
//! the compositor computes it, that the pick's deadline gets the daemon
//! out with no decision, and that the surfaces are gone afterwards.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The globals ADR-0004 requires for a crisp surface.
const NEEDED: [&str; 3] =
    ["zwlr_layer_shell_v1", "wp_fractional_scale_manager_v1", "wp_viewporter"];

/// The two scan rects `App::probe_surfaces` sends, as output-local
/// physical pixels. Kept in step with the daemon on purpose: the whole
/// point is checking the compositor's logical answer against this.
const OUTLINED: [(i32, i32, i32, i32); 2] = [(100, 100, 240, 60), (500, 300, 80, 80)];

/// Each frame is drawn *outside* the rect it marks - a stroke inside
/// would land in the very pixels the next grab reads (ADR-0008,
/// `overlay::scan_marks`) - so the box the compositor is asked to size
/// its surface to is the outset one, two physical px bigger on every
/// side.
const OUTSET: i32 = 2;

/// A private XDG environment plus the daemon running inside it.
struct Session {
    dir: PathBuf,
    log: PathBuf,
    stderr: PathBuf,
    daemon: Child,
}

impl Session {
    fn start() -> Session {
        let display = std::env::var("WAYLAND_DISPLAY").expect("checked by skip()");
        let dir =
            std::env::temp_dir().join(format!("chibipop-surfaces-live-{}", std::process::id()));
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

        // A protocol error is the failure mode these two surfaces are
        // most exposed to - attaching before a configure, asking a
        // device-less seat for a keyboard - and it lands on stderr, not
        // in the log, so it is captured too.
        let stderr = dir.join("stderr.txt");
        let sink = std::fs::File::create(&stderr).expect("creating the stderr capture");

        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &dir);
        let daemon = cmd
            .env("CHIBIPOP_SURFACE_PROBE", "1")
            // The probe needs no pixels, and skipping the ladder keeps
            // the run off the portal's consent dialog.
            .env("CHIBIPOP_CAPTURE_BACKEND", "none")
            .arg("run")
            .stderr(Stdio::from(sink))
            .spawn()
            .expect("spawning the chibipop daemon");
        Session { log: dir.join("state/chibipop/chibipop.log"), stderr, dir, daemon }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait for a line containing `needle`, and answer it.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(line) = self.log().lines().rev().find(|l| l.contains(needle)) {
                return line.to_string();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("waited 20s for {needle:?}; the log was:\n{}", self.log());
    }

    fn terminate(&self) {
        let _ = Command::new("kill").arg("-TERM").arg(self.daemon.id().to_string()).status();
    }

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

/// One `hyprctl` subcommand's output, or `None` off Hyprland.
fn hyprctl(sub: &str) -> Option<String> {
    let out = Command::new("hyprctl").args(["-i", "0", sub]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// `probe: ... on NAME (WxH at S.SSSx)` unpacked.
fn probe_screen(line: &str) -> (i32, i32, f64) {
    let inside = line.split_once('(').and_then(|(_, r)| r.split_once(')')).expect("a (…) clause").0;
    let (size, scale) = inside.split_once(" at ").expect("`WxH at S.SSSx`");
    let (w, h) = size.split_once('x').expect("WxH");
    let scale: f64 =
        scale.trim_end_matches('x').parse().expect("a scale");
    (w.parse().expect("a width"), h.parse().expect("a height"), scale)
}

#[test]
fn the_outline_and_the_selector_map_paint_and_come_back_down_on_a_real_compositor() {
    if skip() {
        return;
    }
    let mut session = Session::start();

    let screen = session.wait_for("probe: CHIBIPOP_SURFACE_PROBE=1 on ");
    let (out_w, out_h, scale) = probe_screen(&screen);
    assert!(scale >= 1.0, "a scale of {scale} is not a scale: {screen}");
    assert!(out_w > 0 && out_h > 0, "the probe must name a real output: {screen}");

    // The outline: click-through and focus-proof, on the overlay layer.
    let mapped = session.wait_for("outline: layer surface ");
    assert!(mapped.contains("overlay layer"), "{mapped}");
    assert!(mapped.contains("keyboard none"), "{mapped}");
    assert!(mapped.contains("click-through"), "{mapped}");
    let shown = session.wait_for("outline: 2 rect(s) on ");
    assert!(shown.contains("2 drawn after clipping"), "both rects land on this output: {shown}");
    session.wait_for("probe: outline shown on 1 surface(s)");

    // The compositor's own view of the outline surface, in logical
    // units: the surface is sized to the *bounding box* of the rects,
    // never to the screen, and the physical->logical derivation is
    // checked against the other side of the protocol rather than
    // against itself.
    let bbox = {
        let x0 = OUTLINED.iter().map(|r| r.0).min().unwrap() - OUTSET;
        let y0 = OUTLINED.iter().map(|r| r.1).min().unwrap() - OUTSET;
        let x1 = OUTLINED.iter().map(|r| r.0 + r.2).max().unwrap() + OUTSET;
        let y1 = OUTLINED.iter().map(|r| r.1 + r.3).max().unwrap() + OUTSET;
        (x0, y0, x1 - x0, y1 - y0)
    };
    if let Some(layers) = hyprctl("layers") {
        let pid = format!("pid: {}", session.daemon.id());
        let line = layers
            .lines()
            .find(|l| l.contains("namespace: chibipop-outline") && l.contains(&pid))
            .unwrap_or_else(|| panic!("no chibipop-outline layer in:\n{layers}"))
            .to_string();
        // `hyprctl` prints `xywh: X Y W H`, every number logical:
        // margins are `round(physical / scale)`, sizes `ceil(...)`.
        let want = format!(
            "xywh: {} {} {} {}",
            (f64::from(bbox.0) / scale).round() as i32,
            (f64::from(bbox.1) / scale).round() as i32,
            (f64::from(bbox.2) / scale).ceil() as i32,
            (f64::from(bbox.3) / scale).ceil() as i32,
        );
        assert!(
            line.contains(&want),
            "the compositor has the outline at {line}, not at {want} \
             (bounding box {bbox:?} physical at {scale}x)"
        );
    }

    // The selector: the exception to ADR-0004's keyboard rule, and the
    // only surface here that asks for focus.
    let picker = session.wait_for("select: 1 full-output surface(s) mapped");
    assert!(picker.contains("overlay layer"), "{picker}");
    assert!(picker.contains("keyboard exclusive"), "{picker}");

    // And the compositor's own answer to "the whole output": the
    // selector asks for `0x0` on four anchors rather than computing a
    // size, so this is the number it must have been handed, checked
    // against the output the probe named. Read from the log rather than
    // `hyprctl` on purpose - the selector's surfaces are destroyed when
    // the pick ends, so polling for them would be a race.
    let configured = session.wait_for("select: surface 0 configured ");
    let want = format!(
        "configured {}x{} logical",
        (f64::from(out_w) / scale).ceil() as i32,
        (f64::from(out_h) / scale).ceil() as i32,
    );
    assert!(
        configured.contains(&want),
        "the selector must cover the whole output: {configured} (wanted {want} for \
         {out_w}x{out_h} physical at {scale}x)"
    );

    // The wedge guard: nothing decided, so the deadline gets the daemon
    // out. On a seat with no pointer there is nothing to drag with and
    // the pick says so instead of waiting - both are the same answer.
    let out = session.wait_for("select: 1 surface(s) down, outcome ");
    assert!(out.contains("outcome cancelled"), "an undriven pick must cancel: {out}");
    let why = session.log();
    assert!(
        why.contains("select: cancelled - the deadline passed with no decision")
            || why.contains("there is nothing to drag with"),
        "the pick must say why it gave up:\n{why}"
    );
    session.wait_for("probe: pick answered false");
    session.wait_for("probe: outline hidden, 0 rect(s) left");

    // ADR-0004's inviolable setting is untouched: the popup still takes
    // no keyboard, whatever the selector asked for.
    let popup = session.wait_for("popup: layer surface 0 on ");
    assert!(popup.contains("keyboard none"), "the popup must never take focus: {popup}");

    // The two most likely ways these surfaces break a session are both
    // protocol errors - attaching a buffer before the first configure,
    // and asking a device-less seat for a keyboard - and either one
    // kills the whole connection.
    session.wait_for("ready: pump running");
    let stderr = session.stderr();
    assert!(!stderr.contains("Protocol error"), "the probe raised a protocol error:\n{stderr}");

    // And nothing is left behind: the surfaces go with the process.
    session.terminate();
    assert!(session.wait_exit(), "the daemon ignored SIGTERM");
    if let Some(layers) = hyprctl("layers") {
        let pid = format!("pid: {}", session.daemon.id());
        assert!(
            !layers.contains(&pid),
            "the daemon left layer surfaces behind:\n{layers}"
        );
    }
}
