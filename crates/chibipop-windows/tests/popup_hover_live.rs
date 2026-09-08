#![cfg(windows)]

use chibipop::config::{Config, TriggerMode};
use std::fs::File;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
    word: HWND,
    font: HFONT,
    cursor: POINT,
}

impl Fixture {
    fn start(mode: TriggerMode, enabled: bool) -> Self {
        let root = std::env::temp_dir().join(format!("chibipop-live-hover-{}", std::process::id()));
        std::fs::create_dir_all(root.join("data")).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let database = root.join("data/chibipop.sqlite");
        chibipop::dict::build::build(&[repo.join("tests/fixtures/yomitan/terms.zip")], &[], &database, &|_| {}).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.execute("UPDATE entry SET glossary = ?1 WHERE entry_id IN (SELECT entry_id FROM term WHERE written = '猫')",
            [r#"[{"type":"structured-content","content":[{"tag":"ruby","content":["食",{"tag":"rt","content":"た"}]},"べる"]}]"#]).unwrap();
        drop(connection);
        std::fs::copy(repo.join("data/deconjugator.json"), root.join("data/deconjugator.json")).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_chibipop"), root.join("chibipop.exe")).unwrap();
        let mut config = Config::default();
        config.trigger.mode = mode;
        config.trigger.trigger_key = "F8".into();
        config.anki.enabled = false;
        config.actions.enabled = false;
        config.popup.sub_popups = enabled;
        config.ocr.language = "ja".into();
        config.save(&root.join("chibipop.toml")).unwrap();
        let mut cursor = POINT::default();
        // SAFETY: The cursor output buffer is initialized and live.
        unsafe { GetCursorPos(&mut cursor).unwrap(); }
        let mut fixture = Self { root, child: None, word: HWND::default(), font: HFONT::default(), cursor };
        // SAFETY: All buffers are live. The fixture owns each created native resource.
        unsafe {
            let mut description = LOGFONTW { lfHeight: -72, ..Default::default() };
            for (target, character) in description.lfFaceName.iter_mut().zip("Yu Gothic UI".encode_utf16()) {
                *target = character;
            }
            fixture.font = CreateFontIndirectW(&description);
            assert!(!fixture.font.is_invalid());
            // SS_CENTER | SS_CENTERIMAGE.
            fixture.word = CreateWindowExW(WS_EX_TOPMOST, w!("STATIC"), w!("猫"),
                WS_POPUP | WS_VISIBLE | WINDOW_STYLE(0x201),
                800, 250, 800, 180, None, None, None, None).unwrap();
            SendMessageW(fixture.word, WM_SETFONT, Some(WPARAM(fixture.font.0 as usize)), Some(LPARAM(1)));
            let _ = UpdateWindow(fixture.word);
        }
        fixture.child = Some(Command::new(fixture.root.join("chibipop.exe")).arg("run")
            .current_dir(&fixture.root).stdin(Stdio::null())
            .stdout(File::create(fixture.root.join("stdout.log")).unwrap())
            .stderr(File::create(fixture.root.join("stderr.log")).unwrap())
            .creation_flags(CREATE_NO_WINDOW.0).spawn().unwrap());
        fixture
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default()
            + &std::fs::read_to_string(self.root.join("stdout.log")).unwrap_or_default()
    }

    fn visible_popups(&self) -> Vec<HWND> {
        let mut found = (self.child.as_ref().unwrap().id(), Vec::new());
        // SAFETY: The callback borrows this tuple only during synchronous enumeration.
        unsafe { EnumWindows(Some(collect_popup), LPARAM(&mut found as *mut _ as isize)).unwrap(); }
        found.1
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        // SAFETY: The fixture owns its window and font. The saved cursor belongs to this desktop.
        unsafe {
            let _ = DestroyWindow(self.word);
            let _ = DeleteObject(self.font.into());
            let _ = SetCursorPos(self.cursor.x, self.cursor.y);
        }
        if !std::thread::panicking() { let _ = std::fs::remove_dir_all(&self.root); }
    }
}

unsafe extern "system" fn collect_popup(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows passes live handles. Its caller owns this synchronous callback context.
    unsafe {
        let found = &mut *(data.0 as *mut (u32, Vec<HWND>));
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        let mut class = [0u16; 64];
        let count = GetClassNameW(hwnd, &mut class);
        if process == found.0 && IsWindowVisible(hwnd).as_bool()
            && String::from_utf16_lossy(&class[..count.max(0) as usize]) == "ChibipopPopupClass" {
            found.1.push(hwnd);
        }
    }
    BOOL(1)
}

fn pause(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        // SAFETY: The current test thread owns the message buffer and fixture window.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "Requires Japanese Windows OCR and moves the real desktop pointer"]
fn hovering_real_ocr_popup_opens_a_visible_child() {
    chibipop_windows::text::capture::init_dpi_awareness().unwrap();
    for mode in [TriggerMode::Press, TriggerMode::Live, TriggerMode::HoldKey, TriggerMode::Toggle] {
        run_hover(mode, true);
    }
    run_hover(TriggerMode::Press, false);
}

struct TriggerRelease;

impl Drop for TriggerRelease {
    fn drop(&mut self) {
        let event = INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_F8, dwFlags: KEYEVENTF_KEYUP, ..Default::default() },
        } };
        // SAFETY: Release only the trigger key that this explicit test may hold.
        unsafe { SendInput(&[event], std::mem::size_of::<INPUT>() as i32); }
    }
}

fn run_hover(mode: TriggerMode, enabled: bool) {
    let mut fixture = Fixture::start(mode, enabled);
    let _release = TriggerRelease;
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.logs().contains("running - hover Japanese text") {
        assert!(fixture.child.as_mut().unwrap().try_wait().unwrap().is_none(), "daemon stopped: {}", fixture.logs());
        assert!(Instant::now() < ready_deadline, "daemon never became ready: {}", fixture.logs());
        pause(Duration::from_millis(50));
    }
    // SAFETY: This explicit desktop test sends one trigger key and moves the real pointer.
    unsafe {
        assert!(SetForegroundWindow(fixture.word).as_bool(), "fixture could not take focus");
    }
    pause(Duration::from_millis(200));
    let mut word_rect = RECT::default();
    // SAFETY: The fixture window is live; its current bounds include window-manager placement.
    unsafe {
        GetWindowRect(fixture.word, &mut word_rect).unwrap();
        SetCursorPos((word_rect.left + word_rect.right) / 2, (word_rect.top + word_rect.bottom) / 2).unwrap();
    }
    pause(Duration::from_millis(100));
    eprintln!("live fixture: {}", fixture.root.display());
    // SAFETY: This explicit desktop test sends one trigger key to the focused fixture.
    unsafe {
        let input = |flags| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_F8, dwFlags: flags, ..Default::default() },
        } };
        if mode != TriggerMode::Live {
            let events = if mode == TriggerMode::HoldKey { vec![input(KEYBD_EVENT_FLAGS(0))] }
                else { vec![input(KEYBD_EVENT_FLAGS(0)), input(KEYEVENTF_KEYUP)] };
            assert_eq!(SendInput(&events, std::mem::size_of::<INPUT>() as i32), events.len() as u32);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let root = loop {
        if let Some(window) = fixture.visible_popups().first() { break *window; }
        assert!(fixture.child.as_mut().unwrap().try_wait().unwrap().is_none(), "daemon stopped: {}", fixture.logs());
        assert!(Instant::now() < deadline, "OCR root never appeared: {}", fixture.logs());
        pause(Duration::from_millis(50));
    };
    let mut rect = RECT::default();
    // SAFETY: The discovered popup remains live. Hiding the fixture exposes the popup completely.
    unsafe { GetWindowRect(root, &mut rect).unwrap(); let _ = ShowWindow(fixture.word, SW_HIDE); }
    for y in (rect.top + 8..rect.bottom - 4).step_by(6) {
        // SAFETY: This test is explicitly enabled only for desktop verification.
        unsafe { SetCursorPos(rect.left + 20, y).unwrap(); }
        pause(Duration::from_millis(450));
        if fixture.visible_popups().len() >= 2 {
            assert!(enabled, "disabled sub-popups opened a child");
            eprintln!("real hover passed: {mode:?}");
            cancel_popups(&fixture, true);
            return;
        }
    }
    assert!(!enabled, "hover never opened a visible child in {mode:?} at root {rect:?}: {}", fixture.logs());
    assert_eq!(fixture.visible_popups().len(), 1, "disabling sub-popups must preserve the root");
    eprintln!("disabled hover passed: {mode:?}");
    cancel_popups(&fixture, false);
}

fn cancel_popups(fixture: &Fixture, child: bool) {
    // SAFETY: Only the fixture's own source window receives the injected Escape keys.
    unsafe {
        SetWindowPos(fixture.word, Some(HWND_TOPMOST), 20, 20, 240, 100, SWP_SHOWWINDOW).unwrap();
        assert!(SetForegroundWindow(fixture.word).as_bool());
    }
    let escape = || {
        let event = |flags| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_ESCAPE, dwFlags: flags, ..Default::default() },
        } };
        // SAFETY: The fixture has focus and both initialized input records are live.
        unsafe { assert_eq!(SendInput(&[event(KEYBD_EVENT_FLAGS(0)), event(KEYEVENTF_KEYUP)],
            std::mem::size_of::<INPUT>() as i32), 2); }
    };
    let wait_count = |count| {
        let deadline = Instant::now() + Duration::from_secs(3);
        while fixture.visible_popups().len() != count {
            assert!(Instant::now() < deadline, "Escape expected {count} popup(s): {}", fixture.logs());
            pause(Duration::from_millis(30));
        }
    };
    if child { escape(); wait_count(1); }
    escape(); wait_count(0);
    pause(Duration::from_millis(250));
    assert!(fixture.visible_popups().is_empty(), "cancelled work reopened the popup");
}
