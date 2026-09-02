//! Tests the stock-GNOME posture on a compositor that has no layer
//! shell.
//!
//! No one can install GNOME beside a session to test against. A claim
//! about "what chibipop does on Mutter" that only ran against a mock is
//! not evidence. The shape of that session is reachable instead. `cage`
//! (wlroots) advertises no `zwlr_layer_shell_v1` and no
//! `wp_fractional_scale_manager_v1`. A nested headless `cage` is
//! therefore a real compositor with the same capability gap as Mutter.
//! This test runs the real daemon inside `cage`. It asserts the complete
//! documented posture in one pass, and it expects four results. A
//! startup diagnostic names the absent global. The Popup channel row
//! names the absent global too. Every other channel still resolves. The
//! settings window still opens, and the daemon still runs at the end.
//! The last result is not theoretical. The daemon panicked here on its
//! first `wl_shm::format`, because a dropped popup left its other
//! Wayland objects to dispatch into handlers with nothing behind them.
//!
//! The session is headless and nested. It takes no seat, it takes no
//! focus, and it draws nothing on a screen. The test skips when `cage`
//! is absent.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The real chibipop binary.
const BIN: &str = env!("CARGO_BIN_EXE_chibipop");

/// The time the nested session gets to start and to open its dictionary.
/// The worker thread opens the OCR models and SQLite at startup.
const READY: Duration = Duration::from_secs(30);

/// A nested compositor and every process inside it. This guard ends all
/// of them. `cage` exits with its child, so a kill of `cage` stops the
/// daemon.
struct Nested {
    cage: Child,
    settings: Option<Child>,
    root: PathBuf,
}

impl Nested {
    /// Start `cage -- chibipop run` on the headless backend in a private
    /// XDG world.
    ///
    /// The session gets its own runtime directory. The lock and the
    /// socket of the daemon can then never collide with the real session
    /// of the developer. The session also gets a session-bus address
    /// that answers nothing. That address gives the half of the
    /// stock-GNOME shape with no tray and no portal.
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
            // real Hyprland outside. A nested session has the shape of
            // niri or river, and its Cursor row must report that shape.
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let cage = cage.spawn().expect("spawning cage");
        Nested { cage, settings: None, root }
    }

    fn log_path(&self) -> PathBuf {
        self.root.join("state/chibipop/chibipop.log")
    }

    /// The log of the daemon, after `needle` appears in it.
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

    /// The display of the nested compositor. This code finds the display
    /// and does not assume it. The scratch runtime directory holds
    /// exactly one wayland socket.
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

/// Point every XDG variable that chibipop reads inside the scratch tree.
/// The set includes the runtime directory, because the nested compositor
/// creates its socket there.
fn xdg(cmd: &mut Command, root: &Path) {
    cmd.env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        // A bus that answers nothing. There is no ScreenCast portal, no
        // GlobalShortcuts, and no StatusNotifier host. That state is
        // stock GNOME without its extensions. Each absence must produce
        // a diagnostic, not a failure.
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

/// Assert the complete documented GNOME posture in one pass. The posture
/// is one thing, and its parts are true only together.
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

    // The capability report names the exact global, and it limits the
    // cost to the hover loop. It does not limit the whole application,
    // which still starts around this line.
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

    // The Popup channel row. A user with no log sees this state only
    // here. The row names the global, so the user can see that an
    // upgrade is the fix.
    assert!(
        log.contains("channel: Popup: unsupported - missing zwlr_layer_shell_v1"),
        "log was:\n{log}"
    );
    // The other three channels still resolve to real verdicts. They give
    // capture with no prompt, a cursor ladder that names the absent
    // global, and the socket that always binds.
    assert!(log.contains("channel: Capture: wlr-screencopy region capture"), "log was:\n{log}");
    assert!(
        log.contains("channel: Cursor: unsupported - missing ext_image_copy_capture_manager_v1"),
        "log was:\n{log}"
    );
    assert!(log.contains("channel: Trigger: control socket"), "log was:\n{log}");
    assert!(log.contains("popup unavailable (no layer shell)"), "log was:\n{log}");

    // The daemon still runs. `cage` exits with its child, so a live cage
    // means a live daemon. This assertion failed before the
    // no-layer-shell fallback existed. At that time the orphaned
    // `wl_shm` of the popup panicked the pump within a second of
    // startup.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        nested.cage.try_wait().expect("polling cage").is_none(),
        "the daemon must survive a compositor with no layer shell; log was:\n{}",
        std::fs::read_to_string(nested.log_path()).unwrap_or_default()
    );

    // The settings window still opens. It is an ordinary xdg-shell
    // window, and it needs nothing from the layer shell
    // (README § Linux → GNOME).
    nested.open_settings();
    std::thread::sleep(Duration::from_secs(5));
    let settings = nested.settings.as_mut().expect("settings child");
    assert!(
        settings.try_wait().expect("polling settings").is_none(),
        "the settings window must open on a compositor with no layer shell"
    );
}
