#![cfg(windows)]

use chibipop::config::{Config, TriggerMode};
use std::fs::{File, Permissions};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Controls::TCM_SETCURFOCUS;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetDlgCtrlID, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SendMessageTimeoutW, SendMessageW, CB_SETCURSEL,
    BM_SETCHECK, CBN_SELCHANGE, SC_CLOSE, SC_MAXIMIZE, SC_RESTORE, SMTO_ABORTIFHUNG, WM_COMMAND,
    WM_GETTEXT, WM_SYSCOMMAND,
};

static SERIAL: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct ProcessFixture {
    root: PathBuf,
    child: Child,
    config_permissions: Permissions,
    expected_language: String,
}

impl ProcessFixture {
    fn start(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!("chibipop_ui_process_{}_{}",
            std::process::id(), NEXT_ID.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(root.join("library")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let archive = root.join("library/terms.zip");
        std::fs::copy(repo.join("tests/fixtures/yomitan/terms.zip"), &archive).unwrap();
        let database = root.join("data/chibipop.sqlite");
        chibipop::dict::build::build(&[archive], &[], &database, &|_| {}).unwrap();
        std::fs::copy(repo.join("data/deconjugator.json"), root.join("data/deconjugator.json")).unwrap();
        let executable = root.join("chibipop.exe");
        std::fs::copy(env!("CARGO_BIN_EXE_chibipop"), &executable).unwrap();
        let mut config = Config::default();
        config.trigger.mode = TriggerMode::HoldKey;
        config.trigger.trigger_key = "0x87".into();
        config.actions.enabled = false;
        config.anki.enabled = false;
        config.anki.url = "http://127.0.0.1:9".into();
        config.ocr.engine = "missing-test-provider".into();
        if let Some((_, language)) = chibipop_windows::text::ocr::installed_recognisers().first() {
            config.ocr.language = language.clone();
        }
        config.save(&root.join("chibipop.toml")).unwrap();
        let expected_language = if mode == "run" {
            let engine = chibipop_windows::text::ocr::WinrtOcr::new(&config.ocr.language).unwrap();
            engine.engine().RecognizerLanguage().unwrap().LanguageTag().unwrap().to_string()
        } else {
            config.ocr.language.clone()
        };
        let config_permissions = std::fs::metadata(root.join("chibipop.toml")).unwrap().permissions();
        let child = Command::new(executable).arg(mode)
            .arg("--config").arg(root.join("chibipop.toml"))
            .arg("--dict").arg(database)
            .current_dir(&root).stdin(Stdio::null())
            .stdout(File::create(root.join("stdout.log")).unwrap())
            .stderr(File::create(root.join("stderr.log")).unwrap())
            .creation_flags(CREATE_NO_WINDOW.0).spawn().unwrap();
        Self { root, child, config_permissions, expected_language }
    }

    fn window(&mut self, title: &str) -> HWND {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let windows = owned_windows(self.child.id(), title);
            if let [window] = windows.as_slice() {
                return *window;
            }
            assert!(windows.len() <= 1, "duplicate {title} windows");
            assert!(self.child.try_wait().unwrap().is_none(), "process stopped: {}", self.logs());
            assert!(Instant::now() < deadline, "missing {title}: {}", self.logs());
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default()
    }

    fn wait_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "exit={status}: {}", self.logs());
                return;
            }
            assert!(Instant::now() < deadline, "X did not exit the process: {}", self.logs());
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("child diagnostics: {}", self.logs());
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::set_permissions(self.root.join("chibipop.toml"), self.config_permissions.clone());
        if self.root.is_absolute() && self.root.starts_with(std::env::temp_dir()) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

struct WindowSearch<'a> {
    process: u32,
    title: &'a str,
    windows: Vec<HWND>,
}

unsafe extern "system" fn find_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumWindows calls synchronously while the caller owns this search.
    unsafe {
        let search = &mut *(parameter.0 as *mut WindowSearch<'_>);
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        if process == search.process && IsWindowVisible(hwnd).as_bool() {
            let mut text = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut text);
            if String::from_utf16_lossy(&text[..len.max(0) as usize]) == search.title {
                search.windows.push(hwnd);
            }
        }
    }
    BOOL(1)
}

fn owned_windows(process: u32, title: &str) -> Vec<HWND> {
    let mut search = WindowSearch { process, title, windows: Vec::new() };
    // SAFETY: The callback borrows this search only during enumeration.
    unsafe { let _ = EnumWindows(Some(find_window), LPARAM(&mut search as *mut _ as isize)); }
    search.windows
}

unsafe extern "system" fn find_control(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumChildWindows calls synchronously with a live search tuple.
    unsafe {
        let search = &mut *(parameter.0 as *mut (i32, Option<HWND>));
        if GetDlgCtrlID(hwnd) == search.0 {
            search.1 = Some(hwnd);
            return BOOL(0);
        }
    }
    BOOL(1)
}

fn control(root: HWND, id: i32) -> HWND {
    let mut search = (id, None);
    // SAFETY: The callback borrows this tuple only during enumeration.
    unsafe { let _ = EnumChildWindows(Some(root), Some(find_control), LPARAM(&mut search as *mut _ as isize)); }
    search.1.unwrap_or_else(|| panic!("missing control {id}"))
}

fn text(control: HWND) -> String {
    let mut text = [0u16; 2048];
    let mut copied = 0usize;
    // SAFETY: WM_GETTEXT is marshalled by Windows between processes. The buffer
    // remains writable for the whole synchronous, time-bounded call.
    unsafe {
        SendMessageTimeoutW(control, WM_GETTEXT, WPARAM(text.len()), LPARAM(text.as_mut_ptr() as isize),
            SMTO_ABORTIFHUNG, 500, Some(&mut copied));
    }
    String::from_utf16_lossy(&text[..copied.min(text.len())])
}

fn wait_until(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "native UI condition timed out: {label}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn system_command(window: HWND, command: u32) {
    // SAFETY: The test selected this window by its owned child process ID.
    unsafe { PostMessageW(Some(window), WM_SYSCOMMAND, WPARAM(command as usize), LPARAM(0)).unwrap(); }
}

#[test]
fn standalone_x_exits_and_reports_inactive_scanning() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let mut process = ProcessFixture::start("settings");
    let window = process.window("chibipop settings");
    let status = text(control(window, 194));
    assert!(status.contains("Not scanning"), "{status}");
    assert!(status.contains("Not running"), "{status}");
    system_command(window, SC_CLOSE);
    process.wait_exit();
}

#[test]
fn audit_keeps_machine_readable_json_and_does_not_enable_capture() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let mut process = ProcessFixture::start("settings");
    let window = process.window("chibipop settings");
    system_command(window, SC_CLOSE);
    process.wait_exit();
    let output = Command::new(process.root.join("chibipop.exe"))
        .args(["settings", "--audit", "--config"]).arg(process.root.join("chibipop.toml"))
        .arg("--dict").arg(process.root.join("data/chibipop.sqlite"))
        .current_dir(&process.root).creation_flags(CREATE_NO_WINDOW.0).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let audit: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let labels: Vec<_> = audit["dumps"].as_array().unwrap().iter()
        .filter(|dump| dump["field_map_expanded"] == false)
        .map(|dump| dump["tab_label"].as_str().unwrap()).collect();
    assert_eq!(vec!["Popup", "Configurations", "Dictionaries", "Text recognition", "Anki", "Extensions", "Debug"], labels);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("live diagnostics enabled"));
}

#[test]
fn daemon_reports_real_ocr_saves_truthfully_opens_logs_and_exits_via_x() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let mut process = ProcessFixture::start("run");
    let window = process.window("chibipop settings");
    wait_until("active OCR status", || text(control(window, 194)).contains("Windows OCR"));
    let runtime = text(control(window, 194));
    assert_eq!(format!("Language: {}", process.expected_language), runtime.split(" | ").next().unwrap());
    assert!(!runtime.contains("missing-test-provider"), "{runtime}");
    assert!(runtime.contains("disabled"), "{runtime}");
    system_command(window, SC_MAXIMIZE);
    // SAFETY: These queries read the selected child-process window.
    wait_until("maximize", || unsafe { IsZoomed(window).as_bool() });
    system_command(window, SC_RESTORE);
    wait_until("restore", || !unsafe { IsZoomed(window).as_bool() });
    let tab = control(window, 130);
    // SAFETY: These messages contain only control identifiers, handles, and integers.
    unsafe {
        SendMessageW(tab, TCM_SETCURFOCUS, Some(WPARAM(6)), None);
    }
    wait_until("Debug controls visible", || unsafe { IsWindowVisible(control(window, 192)).as_bool() });
    // SAFETY: This is the selected child-process window's log command.
    unsafe {
        PostMessageW(Some(window), WM_COMMAND, WPARAM(192), LPARAM(0)).unwrap();
    }
    let viewer = process.window("chibipop live logs");
    wait_until("live log update", || text(control(viewer, 100)).contains("live diagnostics enabled"));
    system_command(viewer, SC_CLOSE);
    wait_until("close log viewer", || !unsafe { IsWindow(Some(viewer)).as_bool() });
    assert!(process.child.try_wait().unwrap().is_none());

    let theme = control(window, 104);
    let anki_enabled = control(window, 125);
    // SAFETY: The controls belong to the selected child process. Messages contain no pointers.
    unsafe {
        SendMessageW(tab, TCM_SETCURFOCUS, Some(WPARAM(0)), None);
        SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(1)), None);
        PostMessageW(Some(window), WM_COMMAND,
            WPARAM(104 | ((CBN_SELCHANGE as usize) << 16)), LPARAM(theme.0 as isize)).unwrap();
        SendMessageW(anki_enabled, BM_SETCHECK, Some(WPARAM(1)), None);
        PostMessageW(Some(window), WM_COMMAND, WPARAM(125), LPARAM(anki_enabled.0 as isize)).unwrap();
    }
    wait_until("pending changes", || text(control(window, 193)).contains("Pending"));
    assert!(text(control(window, 194)).contains("Anki: disabled"));
    let mut readonly = process.config_permissions.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(process.root.join("chibipop.toml"), readonly).unwrap();
    // SAFETY: The Apply command belongs to the selected child-process window.
    unsafe { PostMessageW(Some(window), WM_COMMAND, WPARAM(100), LPARAM(0)).unwrap(); }
    wait_until("save failure", || text(control(window, 193)).contains("Failed"));
    std::fs::set_permissions(process.root.join("chibipop.toml"), process.config_permissions.clone()).unwrap();
    // SAFETY: The same live window owns the retry command.
    unsafe { PostMessageW(Some(window), WM_COMMAND, WPARAM(100), LPARAM(0)).unwrap(); }
    wait_until("save success", || text(control(window, 193)).contains("Applied"));
    wait_until("applied Anki state", || text(control(window, 194)).contains("Anki: enabled"));
    let saved: Config = toml::from_str(&std::fs::read_to_string(process.root.join("chibipop.toml")).unwrap()).unwrap();
    assert_eq!("light", saved.popup.theme);
    assert!(saved.anki.enabled);
    system_command(window, SC_CLOSE);
    process.wait_exit();
}
