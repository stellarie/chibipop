//! This test checks the region selector and outline overlay with a real compositor.
//!
//! CI is headless (ARCHITECTURE.md#packaging-and-ci). The test skips without
//! `WAYLAND_DISPLAY`.
//! It also skips when the compositor lacks layer shell, fractional scale, or viewporter.
//! These globals are required. Their absence is a compositor state, not a test failure.
//!
//! The test drives one daemon with `CHIBIPOP_SURFACE_PROBE=1`.
//! The probe sends two known scan rects through the shipped
//! `Command::ShowScanOverlay` consumer, runs one region pick, and removes both surfaces.
//! The assertions read the daemon log and `hyprctl` output.
//! The test synthesizes no input and never touches the seat.
//!
//! **What this cannot prove.** A drag needs a seat press, motion, and release.
//! This crate does not synthesize seat input (`popup_live.rs`: "it synthesizes no input and
//! never touches the seat").
//! The pointer script exists so popup handlers can run without seat input.
//! This file has no equivalent hook. A script would test the drag arithmetic instead of the
//! compositor.
//! Unit tests in `select.rs` pin the drag itself (`Drag`/`Ask`, physical pixels, negative
//! directions, the threshold, and both cancels).
//! The behavior pinned *here* belongs to the compositor.
//! The surfaces use the correct layer and keyboard interactivity.
//! Geometry follows output fractional scale as the compositor computes it.
//! The pick deadline ends without a decision, and the surfaces disappear.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// Globals required for the surface protocol.
const NEEDED: [&str; 3] =
    ["zwlr_layer_shell_v1", "wp_fractional_scale_manager_v1", "wp_viewporter"];

/// The two scan rects that `App::probe_surfaces` sends in output-local physical pixels.
/// Keep these values equal to the daemon values. The test compares the compositor's logical
/// result against them.
const OUTLINED: [(i32, i32, i32, i32); 2] = [(100, 100, 240, 60), (500, 300, 80, 80)];

/// Draw each frame outside the rect that it marks.
/// A stroke inside the rect would enter the pixels that the next grab reads
/// (ARCHITECTURE.md#capture-and-masking, `overlay::scan_marks`).
/// The compositor therefore receives an outset box that is two physical px larger on each
/// side.
const OUTSET: i32 = 2;

/// A private XDG environment with a daemon.
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

        // These two surfaces can fail with protocol errors when they attach a buffer before
        // configure or ask a device-less seat for a keyboard.
        // The compositor writes these errors to stderr, not the log, so capture stderr too.
        let stderr = dir.join("stderr.txt");
        let sink = std::fs::File::create(&stderr).expect("creating the stderr capture");

        let mut cmd = Command::new(BIN);
        xdg(&mut cmd, &dir);
        let daemon = cmd
            .env("CHIBIPOP_SURFACE_PROBE", "1")
            // The probe needs no pixels. The `none` override prevents a portal consent dialog.
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

    /// Wait for a log line that contains `needle`, then return it.
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

/// Return one `hyprctl` subcommand's output, or `None` outside Hyprland.
fn hyprctl(sub: &str) -> Option<String> {
    let out = Command::new("hyprctl").args(["-i", "0", sub]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Parse `probe: ... on NAME (WxH at S.SSSx)`.
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

    // The outline accepts clicks through itself and never takes focus on the overlay layer.
    let mapped = session.wait_for("outline: layer surface ");
    assert!(mapped.contains("overlay layer"), "{mapped}");
    assert!(mapped.contains("keyboard none"), "{mapped}");
    assert!(mapped.contains("click-through"), "{mapped}");
    let shown = session.wait_for("outline: 2 rect(s) on ");
    assert!(shown.contains("2 drawn after clipping"), "both rects land on this output: {shown}");
    session.wait_for("probe: outline shown on 1 surface(s)");

        // Read the compositor's logical outline surface.
        // The surface uses the rects' *bounding box*, not the screen.
        // Check physical-to-logical conversion against the compositor's result, not itself.
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
        // `hyprctl` prints `xywh: X Y W H` in logical units.
        // Margins use `round(physical / scale)`, and sizes use `ceil(...)`.
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

    // The selector is the one exception to the popup keyboard rule.
    // It is the only surface here that can take focus.
    let picker = session.wait_for("select: 1 full-output surface(s) mapped");
    assert!(picker.contains("overlay layer"), "{picker}");
    assert!(picker.contains("keyboard exclusive"), "{picker}");

    // The selector asks for `0x0` on four anchors instead of a size calculation.
    // The compositor reports the whole output size in the log.
    // Read the log instead of `hyprctl` because the selector surfaces disappear when the pick
    // ends. A `hyprctl` poll could race with that removal.
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

    // No decision arrives, so the deadline ends the pick.
    // A seat without a pointer cannot support a drag. The pick reports the same cancellation
    // instead of a wait.
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

    // The popup keeps its strict keyboard rule.
    // The selector's keyboard mode must not change it.
    let popup = session.wait_for("popup: layer surface 0 on ");
    assert!(popup.contains("keyboard none"), "the popup must never take focus: {popup}");

    // Two protocol errors can end the session: a buffer attach before the first configure, or
    // a keyboard request from a device-less seat.
    // Either error can close the whole connection.
    session.wait_for("ready: pump running");
    let stderr = session.stderr();
    assert!(!stderr.contains("Protocol error"), "the probe raised a protocol error:\n{stderr}");

    // Process exit must remove both surfaces.
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
