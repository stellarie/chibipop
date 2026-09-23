//! This test checks the hidden functional command with the real chibipop.exe.
//!
//! Windows-only: the command lives in the Windows bin, and the no-window proof
//! enumerates Win32 top-level windows. Elsewhere, this file compiles to zero
//! tests.
#![cfg(windows)]

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, EnumWindows, GetWindowThreadProcessId,
    IsWindowVisible, RegisterClassW, ShowWindow, CW_USEDEFAULT, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

/// The command resolves each fixture against the repository root, not against
/// the working directory. The test harness sets the working directory to this
/// package, so a relative path here would resolve to the wrong tree.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The manifest path and the repository root use the same layout. A test that
/// passes a copy of the manifest still names the same repository.
fn manifest_path() -> PathBuf {
    repo_root().join("tests/functional/manifest.json")
}

const TIMEOUT: Duration = Duration::from_secs(300);
const WINDOW_SAMPLE: Duration = Duration::from_millis(20);

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Removes the temporary directory that this test created, even after a panic.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_temp(name: &str) -> (PathBuf, TempDir) {
    let dir = std::env::temp_dir().join(format!(
        "chibipop_functional_{}_{name}_{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), TempDir(dir))
}

fn command(manifest: &Path, case: Option<&str>, run_root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chibipop"));
    command
        .args(["test", "functional", "--manifest"])
        .arg(manifest)
        .arg("--run-root")
        .arg(run_root);
    if let Some(id) = case {
        command.arg("--case").arg(id);
    }
    // Without this flag, the console subsystem gives the child its own console
    // window. That window would break the no-window proof below.
    command.creation_flags(CREATE_NO_WINDOW.0);
    command
}

fn parse_lines(stdout: &[u8], context: &str) -> Vec<Value> {
    let text = String::from_utf8(stdout.to_vec())
        .unwrap_or_else(|error| panic!("{context}: stdout is not UTF-8: {error}"));
    text.lines()
        .map(|line| {
            // stdout carries JSON and nothing else. A reader parses every line,
            // so a stray report line is a contract break, not noise.
            assert!(
                line.starts_with('{'),
                "{context}: stdout holds a line that is not JSON: {line:?}"
            );
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("{context}: not one JSON object per line: {line:?} ({error})"))
        })
        .collect()
}

/// Runs one invocation to completion and returns its output and its lines.
struct Run {
    stdout: String,
    status: std::process::ExitStatus,
    lines: Vec<Value>,
    temp: TempDir,
}

fn run(manifest: &Path, case: Option<&str>, name: &str) -> Run {
    let (root, temp) = fresh_temp(name);
    let run_root = root.join("run-root");
    let output = command(manifest, case, &run_root)
        .output()
        .expect("spawning the functional command");
    let context = format!("{}", String::from_utf8_lossy(&output.stderr));
    let lines = parse_lines(&output.stdout, &context);
    Run {
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        status: output.status,
        lines,
        temp,
    }
}

/// Runs one invocation of the manifest and returns the child that owns it.
fn start(manifest: &Path, run_root: &Path) -> Child {
    command(manifest, None, run_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning the functional command")
}

fn titles(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["id"].as_str().unwrap_or("<missing id>").to_string())
        .collect()
}

fn statuses(lines: &[Value]) -> Vec<(String, String)> {
    lines
        .iter()
        .map(|line| {
            (
                line["id"].as_str().unwrap_or_default().to_string(),
                line["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn one_manifest_invocation_emits_eleven_lines_from_one_process() {
    let full = run(&manifest_path(), None, "manifest");

    assert_eq!(11, full.lines.len(), "lines: {}", full.stdout);
    for line in &full.lines {
        for key in ["id", "title", "status", "detail", "duration_ms", "pid"] {
            assert!(
                !line[key].is_null(),
                "case {:?} has no {key}: {line}",
                line["id"]
            );
        }
    }

    // One process ran every case. One `pid` value in eleven lines is that proof.
    let pids: BTreeSet<u64> = full.lines.iter().filter_map(|line| line["pid"].as_u64()).collect();
    assert_eq!(1, pids.len(), "one process, so one pid: {pids:?} in {}", full.stdout);

    for (id, status) in statuses(&full.lines) {
        assert!(
            matches!(status.as_str(), "PASS" | "SKIP" | "FAIL"),
            "case {id} reports {status:?}"
        );
    }

    // A missing fixture or recogniser is a SKIP, never a FAIL.
    let failed: Vec<(String, String)> = statuses(&full.lines)
        .into_iter()
        .filter(|(_, status)| status == "FAIL")
        .collect();
    assert!(failed.is_empty(), "failed cases: {failed:?}\n{}", full.stdout);
    assert!(full.status.success(), "exit {} for {}\n{}", full.status, titles(&full.lines).join(","), full.stdout);
}

/// A process that owns a visible top-level window. Its window proves that the
/// no-window proof detects a window when one exists, so a zero result means
/// something. `Drop` closes the window, even after a panic.
struct WindowControl(Child);

impl WindowControl {
    fn start() -> WindowControl {
        let script = "Add-Type -AssemblyName System.Windows.Forms;\
                      $f = New-Object System.Windows.Forms.Form;\
                      $f.Text = 'chibipop window control';\
                      $f.Show();\
                      Start-Sleep -Seconds 30";
        let child = Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .expect("spawning the window control");
        let control = WindowControl(child);
        let pid = control.0.id();
        let deadline = Instant::now() + Duration::from_secs(30);
        while visible_windows(pid).is_empty() {
            assert!(
                Instant::now() < deadline,
                "the window control never showed a window; the no-window proof cannot detect one"
            );
            std::thread::sleep(WINDOW_SAMPLE);
        }
        control
    }
}

impl Drop for WindowControl {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_window_proof_detects_a_visible_window() {
    let control = WindowControl::start();
    assert!(
        !visible_windows(control.0.id()).is_empty(),
        "the no-window proof must see the window that pid {} shows",
        control.0.id()
    );
}

#[test]
fn one_case_invocation_emits_one_line() {
    let single = run(&manifest_path(), Some("text-resolution"), "case");

    assert_eq!(1, single.lines.len(), "lines: {}", single.stdout);
    assert_eq!("text-resolution", single.lines[0]["id"]);
    assert_eq!("PASS", single.lines[0]["status"], "{}", single.lines[0]);
    assert!(single.status.success(), "exit {}", single.status);
}

#[test]
fn the_window_enumerator_sees_a_window_and_filters_by_process() {
    // The no-window proof reports zero only when this enumerator works. This
    // control creates one real window: the enumerator must find it while it is
    // visible, and it must not find it while it is hidden.
    let class = format!("chibipop-functional-control-{}", std::process::id());
    let name: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `name` is a null-terminated UTF-16 string that outlives the call.
    // `WNDCLASSW` is fully initialized and only read. `wndproc` is a valid
    // callback for the life of the class, which this test never unregisters.
    let atom = unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&class)
    };
    assert_ne!(0, atom, "RegisterClassW failed");

    // SAFETY: The class name was registered above, and the module handle is
    // borrowed for the call only. `CW_USEDEFAULT` and zero sizes ask for a
    // default frame. The window is created hidden, so nothing appears.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(name.as_ptr()),
            PCWSTR(name.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            200,
            100,
            None,
            None,
            Some(GetModuleHandleW(None).unwrap().into()),
            None,
        )
    }
    .expect("CreateWindowExW");

    let pid = std::process::id();
    // SAFETY: `window` is the live window created above.
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
    }
    assert!(
        visible_windows(pid).contains(&window),
        "the enumerator must find a visible window of this process"
    );

    // SAFETY: `window` is live, and this process owns it.
    unsafe {
        let _ = ShowWindow(window, SW_HIDE);
    }
    assert!(
        !visible_windows(pid).contains(&window),
        "the enumerator must reject a hidden window"
    );

    // The process filter rejects a process that does not own this window.
    let other = visible_windows(pid.wrapping_add(2_000_003));
    assert!(
        !other.contains(&window),
        "the enumerator must filter by the owning process: {other:?}"
    );

    // SAFETY: `window` is live and this process owns it. These calls run once
    // per test, and the class stays registered until the process ends.
    unsafe {
        let _ = DestroyWindow(window);
    }
}

unsafe extern "system" fn wndproc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: The default handler accepts any message for any window.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[test]
fn the_child_process_opens_no_visible_top_level_window() {
    let (_root, temp) = fresh_temp("no-window");
    let run_root = _root.join("run-root");
    let mut child = start(&manifest_path(), &run_root);
    let pid = child.id();

    // Enumeration samples the window list. It shows a window when the sample
    // catches one. Therefore it proves presence and only samples absence: the
    // child could create and destroy a window between two samples.
    let mut samples = 0u32;
    let mut seen: Vec<HWND> = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        for window in visible_windows(pid) {
            if !seen.contains(&window) {
                seen.push(window);
            }
        }
        samples += 1;
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the functional command did not finish");
        std::thread::sleep(WINDOW_SAMPLE);
    };

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        status.success(),
        "exit {status}: {stderr}\n{seen:?} visible window(s) for pid {pid}"
    );
    assert_eq!(
        Vec::<HWND>::new(),
        seen,
        "{} of {samples} samples saw a visible top-level window for pid {pid}",
        seen.len()
    );
    assert!(samples > 1, "the proof needs at least two samples, got {samples}");
    drop(temp);
}

#[test]
fn a_failing_case_reports_one_line_and_exits_one() {
    // A manifest that names a case this command does not implement must fail
    // loudly. Swap one id for a case that exists only in the manifest.
    let (_root, temp) = fresh_temp("failing");
    let manifest = temp.0.join("tests/functional/manifest.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    let mut parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path()).unwrap()).unwrap();
    let cases = parsed["cases"].as_array_mut().unwrap();
    let victim = cases
        .iter_mut()
        .find(|case| case["id"] == "text-resolution")
        .expect("the manifest declares text-resolution");
    victim["id"] = Value::from("text-resolution-missing");
    std::fs::write(&manifest, serde_json::to_string_pretty(&parsed).unwrap()).unwrap();

    let failed = run(&manifest, None, "failing");
    let reported = statuses(&failed.lines);
    assert_eq!(11, failed.lines.len(), "lines: {}", failed.stdout);
    assert_eq!(
        vec![("text-resolution-missing".to_string(), "FAIL".to_string())],
        reported
            .into_iter()
            .filter(|(_, status)| status == "FAIL")
            .collect::<Vec<_>>(),
        "exactly the unknown case fails: {}",
        failed.stdout
    );
    assert_eq!(
        Some(1),
        failed.status.code(),
        "one FAIL makes the command exit 1: {}",
        failed.stdout
    );
}

#[test]
fn cases_write_only_under_the_run_root() {
    let before = git_status();
    let full = run(&manifest_path(), None, "run-root");
    assert!(full.status.success(), "exit {}: {}", full.status, full.stdout);

    let scope = full.temp.0.join("run-root");
    let files = collect_files(&scope);
    assert!(
        files.len() >= 3,
        "the run root holds the case artifacts: {files:?}"
    );
    for name in [
        "data/chibipop.sqlite",
        "cases/png-encoding/artifacts/screenshot.png",
    ] {
        let path = scope.join(name);
        assert!(path.is_file(), "{} is missing from {files:?}", path.display());
        assert!(std::fs::metadata(&path).unwrap().len() > 0, "{} is empty", path.display());
    }

    // Every case reports its own run root, so a write outside the root is visible.
    for line in &full.lines {
        let detail = line["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("RUN_ROOT="), "case {} hides its root: {detail}", line["id"]);
    }

    // The command writes nothing back into the repository.
    assert_eq!(before, git_status(), "the functional command changed the repository");
}

#[test]
fn an_absent_fixture_tree_reports_skip_not_fail() {
    // The command resolves each fixture against the repository root, which it
    // derives from the manifest path. A manifest in an empty tree therefore
    // names no fixture at all.
    let (_root, temp) = fresh_temp("missing");
    let manifest = temp.0.join("tests/functional/manifest.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::copy(manifest_path(), &manifest).unwrap();

    let missing = run(&manifest, None, "absent");
    let reported = statuses(&missing.lines);
    assert_eq!(11, missing.lines.len(), "lines: {}", missing.stdout);
    assert_eq!(
        case_ids(),
        reported.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        "the manifest order is stable"
    );

    let requires = declared_requirements();
    for ((id, status), line) in reported.iter().zip(&missing.lines) {
        let expected = if requires[id].is_empty() { "PASS" } else { "SKIP" };
        assert_eq!(
            expected, status,
            "case {id} declares {:?} and must report {expected}, not {status}: {line}",
            requires[id]
        );
        if expected == "SKIP" {
            assert!(
                line["detail"].as_str().unwrap_or_default().contains("absent"),
                "case {id} must name the absent fixture: {line}"
            );
        }
    }
    assert!(
        missing.status.success(),
        "an all-SKIP run is not a failure: exit {}",
        missing.status
    );
}

/// Returns the required files of each manifest case, keyed by case id.
fn declared_requirements() -> BTreeMap<String, Vec<String>> {
    let text = std::fs::read_to_string(manifest_path()).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    manifest["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let required: Vec<String> = case["requires"]
                .as_array()
                .unwrap()
                .iter()
                .map(|path| path.as_str().unwrap().to_string())
                .collect();
            (case["id"].as_str().unwrap().to_string(), required)
        })
        .collect()
}

/// Returns the case ids of the committed manifest, in order.
fn case_ids() -> Vec<String> {
    let text = std::fs::read_to_string(manifest_path()).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    manifest["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["id"].as_str().unwrap().to_string())
        .collect()
}

fn git_status() -> Vec<String> {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo_root())
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .expect("git status");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn collect_files(root: &Path) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                found.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found.into_iter().collect()
}

struct WindowSearch {
    process: u32,
    windows: Vec<HWND>,
}

/// Enumerates the visible top-level windows that the process owns.
fn visible_windows(process: u32) -> Vec<HWND> {
    let mut search = WindowSearch { process, windows: Vec::new() };
    // SAFETY: EnumWindows calls this callback synchronously. The callback
    // borrows `search`, which outlives the call and no other code mutates it.
    unsafe {
        let _ = EnumWindows(Some(find_window), LPARAM(&mut search as *mut _ as isize));
    }
    search.windows
}

unsafe extern "system" fn find_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: EnumWindows passes the pointer from `visible_windows`, and that
    // function keeps the pointee alive and exclusive for the whole call.
    unsafe {
        let search = &mut *(parameter.0 as *mut WindowSearch);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == search.process && IsWindowVisible(hwnd).as_bool() {
            search.windows.push(hwnd);
        }
    }
    BOOL(1)
}
