//! Each mode gets a fresh native host so UI Automation provider state from a
//! destroyed fixture cannot affect the next run. The test verifies real
//! selection reads and dictionary popups, including the clipboard boundary.

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
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId, CREATE_NO_WINDOW};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
    word: HWND,
    host: HWND,
    font: HFONT,
    cursor: POINT,
}

impl Fixture {
    fn start(mode: TriggerMode) -> Self {
        let root = std::env::temp_dir().join(format!("chibipop-live-selection-{}", std::process::id()));
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
        config.actions.enabled = true;
        config.actions.search.selected_hotkey = Some("G".into());
        config.actions.search.selected_opens_sentence_search = std::env::var("CHIBIPOP_SELECTED_TEST_MODE").as_deref() == Ok("Sentence");
        config.popup.sub_popups = true;
        config.ocr.language = "ja".into();
        config.save(&root.join("chibipop.toml")).unwrap();
        let mut cursor = POINT::default();
        // SAFETY: The cursor output buffer is initialized and live.
        unsafe { GetCursorPos(&mut cursor).unwrap(); }
        let mut fixture = Self { root, child: None, word: HWND::default(), host: HWND::default(), font: HFONT::default(), cursor };
        // SAFETY: All buffers are live. The fixture owns each created native resource.
        unsafe {
            let mut description = LOGFONTW { lfHeight: -72, ..Default::default() };
            for (target, character) in description.lfFaceName.iter_mut().zip("Yu Gothic UI".encode_utf16()) {
                *target = character;
            }
            fixture.font = CreateFontIndirectW(&description);
            assert!(!fixture.font.is_invalid());
            fixture.host = CreateWindowExW(WS_EX_TOPMOST, w!("STATIC"), w!("Chibipop selection regression"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                800, 250, 840, 240, None, None, None, None).unwrap();
            let initial_text = if config.actions.search.selected_opens_sentence_search { w!("猫がいる。") } else { w!("猫") };
            fixture.word = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), initial_text,
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x0004),
                0, 0, 800, 180, Some(fixture.host), None, None, None).unwrap();
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

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default()
    }

    fn visible_popups(&self) -> Vec<HWND> {
        self.owned_windows("ChibipopPopupClass")
    }

    fn owned_windows(&self, class: &str) -> Vec<HWND> {
        let mut found = (self.child.as_ref().unwrap().id(), class, Vec::new());
        // SAFETY: The callback borrows this tuple only during synchronous enumeration.
        unsafe { EnumWindows(Some(collect_popup), LPARAM(&mut found as *mut _ as isize)).unwrap(); }
        found.2
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
            let _ = DestroyWindow(self.host);
            let _ = DeleteObject(self.font.into());
            let _ = SetCursorPos(self.cursor.x, self.cursor.y);
        }
        if !std::thread::panicking() { let _ = std::fs::remove_dir_all(&self.root); }
    }
}

unsafe extern "system" fn collect_popup(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows passes live handles. Its caller owns this synchronous callback context.
    unsafe {
        let found = &mut *(data.0 as *mut (u32, &str, Vec<HWND>));
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        let mut class = [0u16; 64];
        let count = GetClassNameW(hwnd, &mut class);
        if process == found.0 && IsWindowVisible(hwnd).as_bool()
            && String::from_utf16_lossy(&class[..count.max(0) as usize]) == found.1 {
            found.2.push(hwnd);
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
#[ignore = "Requires an interactive Windows desktop and sends keys to its own editor fixture"]
fn selected_editor_text_opens_dictionary_without_ocr() {
    chibipop_windows::text::capture::init_dpi_awareness().unwrap();
    if let Ok(selected) = std::env::var("CHIBIPOP_SELECTED_TEST_MODE") {
        let mode = match selected.as_str() {
            "HoldKey" => TriggerMode::HoldKey,
            "Toggle" => TriggerMode::Toggle,
            "Press" => TriggerMode::Press,
            "Live" => TriggerMode::Live,
            "Sentence" => TriggerMode::HoldKey,
            _ => panic!("unknown selected-text test mode"),
        };
        run_selection(mode);
        return;
    }
    for mode in ["HoldKey", "Toggle", "Press", "Live", "Sentence"] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "selected_editor_text_opens_dictionary_without_ocr", "--ignored", "--nocapture", "--test-threads=1"])
            .env("CHIBIPOP_SELECTED_TEST_MODE", mode)
            .creation_flags(CREATE_NO_WINDOW.0).status().unwrap();
        assert!(status.success(), "selected-text desktop mode failed: {mode}");
    }
}

fn press(key: VIRTUAL_KEY) {
    let event = |flags| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 {
        ki: KEYBDINPUT { wVk: key, dwFlags: flags, ..Default::default() },
    } };
    // SAFETY: The explicitly enabled test focuses its own editor before sending keys.
    unsafe { assert_eq!(SendInput(&[event(KEYBD_EVENT_FLAGS(0)), event(KEYEVENTF_KEYUP)],
        std::mem::size_of::<INPUT>() as i32), 2); }
}

fn run_selection(mode: TriggerMode) {
    let mut fixture = Fixture::start(mode);
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.logs().contains("running - hover Japanese text")
        || fixture.owned_windows("ChibipopSettingsClass").is_empty() {
        assert!(fixture.child.as_mut().unwrap().try_wait().unwrap().is_none(), "daemon stopped: {}", fixture.logs());
        assert!(Instant::now() < ready_deadline, "daemon never became ready: {}", fixture.logs());
        pause(Duration::from_millis(50));
    }
    pause(Duration::from_millis(500));
    let focus_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // SAFETY: The test owns its host and temporarily joins the foreground
        // input queue only for activation. Every successful attach is detached
        // before the loop checks focus or any assertion can unwind.
        unsafe {
            let current = GetCurrentThreadId();
            let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let attached = foreground != 0 && foreground != current
                && AttachThreadInput(current, foreground, true).as_bool();
            let _ = SetForegroundWindow(fixture.host);
            if attached { let _ = AttachThreadInput(current, foreground, false); }
            if GetForegroundWindow() == fixture.host { break; }
        }
        assert!(Instant::now() < focus_deadline, "fixture could not take focus");
        pause(Duration::from_millis(50));
    }

    // SAFETY: The fixture owns this live editor and sets its own selection.
    let clipboard_before = unsafe {
        let _ = SetFocus(Some(fixture.word));
        SendMessageW(fixture.word, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
        windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
    };
    let mut bounds_reader = chibipop_windows::action::selected_text::Reader::new();
    bounds_reader.request(chibipop::controller::RequestId(1000));
    let deadline = Instant::now() + Duration::from_secs(3);
    let selection = loop {
        if let Some((_, selection)) = bounds_reader.poll() { break selection.expect("fixture selection"); }
        assert!(Instant::now() < deadline, "fixture bounds read timed out");
        pause(Duration::from_millis(10));
    };
    let bounds = selection.bounds.expect("fixture selection must expose screen bounds");
    assert!(bounds.w > 1 && bounds.h > 1);
    let before = fixture.diagnostics().len();
    // SAFETY: These read-only queries verify the fixture is still the input target.
    unsafe {
        assert_eq!(GetForegroundWindow(), fixture.host);
        assert_eq!(GetFocus(), fixture.word);
    }
    press(VIRTUAL_KEY(0x47));
    if std::env::var("CHIBIPOP_SELECTED_TEST_MODE").as_deref() == Ok("Sentence") {
        let deadline = Instant::now() + Duration::from_secs(8);
        let search = loop {
            if let Some(window) = fixture.owned_windows("ChibipopSearchWindow").first() { break *window; }
            assert!(Instant::now() < deadline, "sentence search did not open: {}", fixture.logs());
            pause(Duration::from_millis(30));
        };
        // SAFETY: The test discovered its own daemon's search window. WM_GETTEXT
        // is system-marshalled and the output buffer outlives the bounded call.
        unsafe {
            let mut title = [0u16; 64];
            let length = GetWindowTextW(search, &mut title);
            assert_eq!(String::from_utf16_lossy(&title[..length as usize]), "Sentence search");
            let input = GetDlgItem(Some(search), 100).unwrap();
            let mut text = [0u16; 32];
            let mut count = 0usize;
            assert_ne!(SendMessageTimeoutW(input, WM_GETTEXT, WPARAM(text.len()), LPARAM(text.as_mut_ptr() as isize),
                SMTO_ABORTIFHUNG | SMTO_BLOCK, 1000, Some(&mut count)).0, 0);
            assert_eq!(String::from_utf16_lossy(&text[..count]), "猫がいる。");
        }
        assert!(fixture.visible_popups().is_empty());
        assert!(!fixture.diagnostics()[before..].contains("action=request_drill_down"));
        eprintln!("selected sentence search passed");
        return;
    }
    pause(Duration::from_millis(50));
    // SAFETY: The fixture owns this editor and the output buffer.
    unsafe {
        assert_eq!(GetForegroundWindow(), fixture.host, "source lost foreground during lookup");
        assert_eq!(GetFocus(), fixture.word, "source lost edit focus during lookup");
        let mut text = [0u16; 16];
        let length = GetWindowTextW(fixture.word, &mut text);
        assert_eq!(String::from_utf16_lossy(&text[..length as usize]), "猫", "shortcut replaced the source selection");
        assert_eq!(SendMessageW(fixture.word, 0x00B0, None, None).0, 1 << 16, "shortcut changed the source selection");
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while fixture.visible_popups().is_empty() {
        assert!(Instant::now() < deadline, "selection popup absent in {mode:?}: {}", fixture.logs());
        pause(Duration::from_millis(30));
    }
    pause(Duration::from_millis(350));
    assert_eq!(fixture.visible_popups().len(), 1, "selection popup must persist in {mode:?}");
    let mut popup_rect = RECT::default();
    // SAFETY: The popup was discovered in this fixture's live daemon.
    unsafe { GetWindowRect(fixture.visible_popups()[0], &mut popup_rect).unwrap(); }
    assert!(popup_rect.bottom <= bounds.y || popup_rect.top >= bounds.y + bounds.h,
        "popup covered the selected text: {popup_rect:?} vs {bounds:?}");
    let logs = fixture.diagnostics();
    assert!(logs[before..].contains("mode=drill_down"), "must use dictionary-only worker: {logs}");
    assert!(!logs[before..].contains("stage=ocr"), "selection invoked OCR: {logs}");
    // SAFETY: This read-only query does not open or modify the clipboard.
    assert_eq!(unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }, clipboard_before);
    // SAFETY: This collapses only the fixture editor's own selection.
    unsafe { SendMessageW(fixture.word, 0x00B1, Some(WPARAM(0)), Some(LPARAM(0))); }
    let before = fixture.diagnostics().len();
    press(VIRTUAL_KEY(0x47));
    pause(Duration::from_millis(2500));
    assert!(fixture.visible_popups().is_empty(), "empty selection retained an old popup");
    assert!(!fixture.diagnostics()[before..].contains("mode=drill_down"), "empty selection used stale text");
    // SAFETY: The same fixture editor still owns this text and selection.
    unsafe { SendMessageW(fixture.word, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1))); }
    press(VIRTUAL_KEY(0x47));
    let deadline = Instant::now() + Duration::from_secs(8);
    while fixture.visible_popups().is_empty() {
        assert!(Instant::now() < deadline, "repeat selection popup absent: {}", fixture.logs());
        pause(Duration::from_millis(30));
    }
    for (down, up, data) in [(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1), (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2)] {
        let mouse = |flags| INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dwFlags: flags, mouseData: data, ..Default::default() } } };
        // SAFETY: All input targets this fixture's selected word, outside the
        // non-overlapping popup. The initialized events remain live during send.
        unsafe {
            SetCursorPos(bounds.x + bounds.w / 2, bounds.y + bounds.h / 2).unwrap();
            assert_eq!(SendInput(&[mouse(down)], std::mem::size_of::<INPUT>() as i32), 1);
        }
        pause(Duration::from_millis(200));
        assert!(fixture.visible_popups().is_empty(), "outside button did not close popup: {down:?}");
        eprintln!("outside button passed: {down:?}");
        // SAFETY: Release this test's button before queuing Escape. Both events
        // precede message dispatch, so an Edit context menu cannot block the test.
        unsafe { assert_eq!(SendInput(&[mouse(up)], std::mem::size_of::<INPUT>() as i32), 1); }
        press(VK_ESCAPE);
        pause(Duration::from_millis(80));
        // SAFETY: Reset selection in the same owned editor after its outside click.
        unsafe { let _ = SetFocus(Some(fixture.word)); SendMessageW(fixture.word, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1))); }
        press(VIRTUAL_KEY(0x47));
        let deadline = Instant::now() + Duration::from_secs(8);
        while fixture.visible_popups().is_empty() {
            assert!(Instant::now() < deadline, "lookup after outside click failed: {}", fixture.logs());
            pause(Duration::from_millis(30));
        }
    }
    press(VK_ESCAPE);
    pause(Duration::from_millis(250));
    assert!(fixture.visible_popups().is_empty(), "Escape must dismiss selection popup");
    eprintln!("selected editor passed: {mode:?}");
}
