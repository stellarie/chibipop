//! The stock-GNOME posture, on a compositor that really has no layer
//! shell (ticket 49).
//!
//! GNOME cannot be installed beside a session to test against, and a
//! claim about "what chibipop does on Mutter" that only ever ran against
//! a mock is not evidence. What *is* reachable is the shape of that
//! session: `cage` (wlroots) advertises no `zwlr_layer_shell_v1` and no
//! `wp_fractional_scale_manager_v1`, so a nested headless `cage` is a
//! real compositor exhibiting exactly the capability gap Mutter has.
//! This test runs the real daemon inside one and asserts the whole
//! documented posture at once: a startup diagnostic naming the missing
//! global, a Popup channel row that says so, every other channel still
//! resolved, a settings window that still opens - and a daemon that is
//! still running at the end of it. That last one is not theoretical:
//! before this ticket the daemon panicked on its first `wl_shm::format`
//! here, because dropping the popup left its other Wayland objects
//! dispatching into handlers with nothing behind them.
//!
//! Headless and nested, so it takes no seat, steals no focus and puts
//! nothing on anyone's screen. It skips when `cage` is not installed.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// How long the nested session gets to come up and open its dictionary
/// (the OCR models and SQLite open on the worker thread at startup).
const READY: Duration = Duration::from_secs(30);

/// A nested compositor plus everything started inside it, all of which
/// dies with this guard - `cage` exits when its child does, so killing
/// it is what stops the daemon.
struct Nested {
    cage: Child,
    settings: Option<Child>,
    root: PathBuf,
}

impl Nested {
    /// Start `cage -- chibipop run` on the headless backend, in its own
    /// XDG world: its own runtime dir (so the daemon's lock and socket
    /// can never collide with the developer's real session) and a
    /// session-bus address that answers nothing, which is the trayless,
    /// portal-less half of the stock-GNOME shape.
    fn start(root: PathBuf) -> Nested {
        let run = root.join("run");
        std::fs::create_dir_all(&run).expect("scratch runtime dir");
        std::fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .expect("runtime dir must be private");
        for dir in ["config", "data", "state", "cache"] {
            std::fs::create_dir_all(root.join(dir)).expect("scratch xdg dir");
        }

        let mut cage = Command::new("cage");
        cage.arg("--").arg(BIN).arg("run");
        xdg(&mut cage, &root);
        cage.env("WLR_BACKENDS", "headless")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            // Rung 3 of the cursor ladder must not answer through the
            // real Hyprland outside: a nested session is niri/river
            // shaped, and its Cursor row has to say so.
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let cage = cage.spawn().expect("spawning cage");
        Nested { cage, settings: None, root }
    }

    fn log_path(&self) -> PathBuf {
        self.root.join("state/chibipop/chibipop.log")
    }

    /// The daemon's log once `needle` shows up in it.
    fn log_until(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + READY;
        loop {
            let text = std::fs::read_to_string(self.log_path()).unwrap_or_default();
            if text.contains(needle) {
                return text;
            }
            if let Some(status) = self.cage.try_wait().expect("polling cage") {
                panic!("the nested session died ({status}) before {needle:?}; log was:\n{text}");
            }
            if Instant::now() > deadline {
                panic!("{needle:?} never appeared within {READY:?}; log was:\n{text}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The nested compositor's display, discovered rather than assumed:
    /// the scratch runtime dir holds exactly one wayland socket.
    fn display(&self) -> String {
        std::fs::read_dir(self.root.join("run"))
            .expect("runtime dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
            .expect("cage created no wayland socket")
    }

    /// Open the settings window inside the nested session.
    fn open_settings(&mut self) {
        let display = self.display();
        let mut cmd = Command::new(BIN);
        cmd.arg("settings");
        xdg(&mut cmd, &self.root);
        cmd.env("WAYLAND_DISPLAY", display)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.settings = Some(cmd.spawn().expect("spawning chibipop settings"));
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        if let Some(settings) = &mut self.settings {
            let _ = settings.kill();
            let _ = settings.wait();
        }
        let _ = self.cage.kill();
        let _ = self.cage.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every XDG variable chibipop reads, pointed inside the scratch tree -
/// including the runtime dir, which is where the nested compositor's
/// socket lands.
fn xdg(cmd: &mut Command, root: &Path) {
    cmd.env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        // A bus that answers nothing: no ScreenCast portal, no
        // GlobalShortcuts, no StatusNotifier host. That is stock GNOME
        // without its extensions, and every one of those absences is
        // supposed to be a diagnostic rather than a failure.
        .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", root.join("no-bus").display()));
}

fn skip() -> bool {
    if which("cage").is_none() {
        eprintln!("skipping: cage is not installed, so there is no layer-shell-less compositor to run in");
        return true;
    }
    false
}

fn which(exe: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join(exe)).find(|candidate| candidate.is_file())
    })
}

/// The whole documented GNOME posture in one pass, because it is one
/// posture: the parts are only true together.
#[test]
fn a_compositor_without_the_layer_shell_degrades_instead_of_failing() {
    if skip() {
        return;
    }
    let root =
        std::env::temp_dir().join(format!("chibipop-no-layer-shell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut nested = Nested::start(root);
    let log = nested.log_until("ready: pump running");

    // The capability report names the exact global and prices it at the
    // hover loop - not at the whole app, which is still starting up
    // around this very line.
    let missing = log
        .lines()
        .find(|l| l.contains("MISSING"))
        .unwrap_or_else(|| panic!("no MISSING line; log was:\n{log}"));
    assert!(missing.contains("zwlr_layer_shell_v1"), "{missing}");
    assert!(missing.contains("hover loop is unsupported"), "{missing}");
    assert_eq!(
        1,
        log.lines().filter(|l| l.contains("MISSING")).count(),
        "only the layer shell is missing here; log was:\n{log}"
    );
    assert!(!log.contains("cannot run"), "the daemon is running; log was:\n{log}");

    // The Popup channel row, which is the one place a user without a
    // log ever sees this: it names the global, so an upgrade is an
    // obvious fix.
    assert!(
        log.contains("channel: Popup: unsupported - missing zwlr_layer_shell_v1"),
        "log was:\n{log}"
    );
    // ...and the other three channels still resolved to real verdicts:
    // promptless capture, a cursor ladder that names what it lacks, and
    // the always-bound socket.
    assert!(log.contains("channel: Capture: wlr-screencopy region capture"), "log was:\n{log}");
    assert!(
        log.contains("channel: Cursor: unsupported - missing ext_image_copy_capture_manager_v1"),
        "log was:\n{log}"
    );
    assert!(log.contains("channel: Trigger: control socket"), "log was:\n{log}");
    assert!(log.contains("popup unavailable (no layer shell)"), "log was:\n{log}");

    // The daemon is still up. `cage` exits with its child, so a live
    // cage IS a live daemon - and this is the assertion that would have
    // failed before ticket 49, when the popup's orphaned `wl_shm`
    // panicked the pump within a second of startup.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        nested.cage.try_wait().expect("polling cage").is_none(),
        "the daemon must survive a compositor with no layer shell; log was:\n{}",
        std::fs::read_to_string(nested.log_path()).unwrap_or_default()
    );

    // And the settings window still opens: it is an ordinary xdg-shell
    // window and owes the layer shell nothing (README § Linux → GNOME).
    nested.open_settings();
    std::thread::sleep(Duration::from_secs(5));
    let settings = nested.settings.as_mut().expect("settings child");
    assert!(
        settings.try_wait().expect("polling settings").is_none(),
        "the settings window must open on a compositor with no layer shell"
    );
}
