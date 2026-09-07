use crate::diagnostics;
use anyhow::{anyhow, Context, Result};
use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use windows::core::{w, Error, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, UpdateWindow, DEFAULT_GUI_FONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{EM_GETSEL, EM_SCROLLCARET, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetDlgItem, GetMessageW, GetScrollInfo, IsIconic, KillTimer, LoadCursorW, MoveWindow,
    PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowTextW, ShowWindow,
    TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY,
    HMENU, IDC_ARROW, MSG, SB_HORZ, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE,
    SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_NCDESTROY, WM_SETFONT,
    WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
    WS_HSCROLL, WS_OVERLAPPEDWINDOW, WS_VISIBLE, WS_VSCROLL,
};

const EDIT_ID: i32 = 100;
const REFRESH_TIMER: usize = 1;
const REFRESH_MS: u32 = 200;
const START_TIMEOUT: Duration = Duration::from_secs(5);
const WM_FOCUS_VIEWER: u32 = WM_APP + 1;

static CLASS_REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
static VIEWER: Mutex<Option<ViewerRegistration>> = Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static SHOW_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct ViewerRegistration {
    generation: u64,
    hwnd: isize,
}

thread_local! {
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static REVISION: Cell<Option<u64>> = const { Cell::new(None) };
    static CLOSED: Cell<bool> = const { Cell::new(false) };
    static TAIL_HORZ: Cell<i32> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Default)]
struct StartupHooks {
    created: Option<mpsc::Sender<isize>>,
    ready: Option<mpsc::Sender<()>>,
    before_ready: Option<mpsc::Receiver<()>>,
    before_exit: Option<mpsc::Receiver<()>>,
    exited: Option<mpsc::Sender<()>>,
}

/// Opens or focuses live logs.
pub fn show() -> Result<()> {
    show_with_timeout(START_TIMEOUT, #[cfg(test)] StartupHooks::default())
}

fn show_with_timeout(timeout: Duration, #[cfg(test)] hooks: StartupHooks) -> Result<()> {
    let lock = SHOW_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    {
        let mut slot = VIEWER.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(viewer) = slot.as_ref() {
            // SAFETY: Publication and destruction use this lock. The message
            // is queued without synchronously calling the owning UI thread.
            unsafe {
                PostMessageW(Some(hwnd_from_raw(viewer.hwnd)), WM_FOCUS_VIEWER,
                    WPARAM(0), LPARAM(0))
            }.context("focusing the live log window")?;
            return Ok(());
        }
        *slot = Some(ViewerRegistration { generation, hwnd: 0 });
    }

    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    let (activate_tx, activate_rx) = mpsc::sync_channel(1);
    let spawned = thread::Builder::new()
        .name("chibipop-log-window".into())
        .spawn(move || viewer_thread(generation, startup_tx, activate_rx, #[cfg(test)] hooks));
    if let Err(error) = spawned {
        clear_generation(generation);
        return Err(error).context("starting the live log window");
    }
    let result = match startup_rx.recv_timeout(timeout) {
        Ok(Ok(raw)) => {
            let mut slot = VIEWER.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(viewer) = slot.as_mut().filter(|viewer| viewer.generation == generation) {
                viewer.hwnd = raw;
                activate_tx.send(()).context("activating the live log window")
            } else {
                Err(anyhow!("live log window startup was cancelled"))
            }
        }
        Ok(Err(message)) => Err(anyhow!(message)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("live log window startup timed out")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow!("live log window stopped during startup"))
        }
    };
    if result.is_err() {
        clear_generation(generation);
    }
    result
}

#[cfg(test)]
fn active_window() -> Option<HWND> {
    VIEWER.lock().unwrap_or_else(|error| error.into_inner()).as_ref()
        .filter(|viewer| viewer.hwnd != 0).map(|viewer| hwnd_from_raw(viewer.hwnd))
}

fn clear_generation(generation: u64) {
    let mut slot = VIEWER.lock().unwrap_or_else(|error| error.into_inner());
    if slot.as_ref().is_some_and(|viewer| viewer.generation == generation) {
        *slot = None;
    }
}

fn viewer_thread(
    generation: u64,
    startup: mpsc::SyncSender<std::result::Result<isize, String>>,
    activate: mpsc::Receiver<()>,
    #[cfg(test)] hooks: StartupHooks,
) {
    GENERATION.set(generation);
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = startup.send(Err(format!("opening the live log window: {error:#}")));
            return;
        }
    };
    #[cfg(test)]
    {
        if let Some(created) = hooks.created {
            let _ = created.send(hwnd_raw(hwnd));
        }
        if let Some(before_ready) = hooks.before_ready {
            let _ = before_ready.recv();
        }
    }
    if startup.send(Ok(hwnd_raw(hwnd))).is_ok() && activate.recv().is_ok() {
        // SAFETY: This thread owns `hwnd`. Only acknowledged startups can show it.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        refresh_editor(hwnd);
        // SAFETY: The acknowledged viewer remains live on this thread.
        let _ = unsafe { UpdateWindow(hwnd) };
        #[cfg(test)]
        if let Some(ready) = hooks.ready {
            let _ = ready.send(());
        }
        run_message_loop();
    }
    if !CLOSED.get() {
        // SAFETY: This thread owns the window, and WM_NCDESTROY has not run.
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    #[cfg(test)]
    if let Some(before_exit) = hooks.before_exit {
        let _ = before_exit.recv();
    }
    clear_generation(generation);
    #[cfg(test)]
    if let Some(exited) = hooks.exited {
        let _ = exited.send(());
    }
}

fn create_window() -> Result<HWND> {
    // SAFETY: The module handle belongs to this process. Window creation copies
    // the title and class strings before returning.
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)
            .context("GetModuleHandleW(None)")?
            .into();
        register_class(instance)?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name(),
            w!("chibipop live logs"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            820,
            560,
            None,
            None,
            Some(instance),
            None,
        )
        .context("CreateWindowExW for live logs")
    }
}

fn register_class(instance: HINSTANCE) -> Result<()> {
    let registration = CLASS_REGISTERED.get_or_init(|| {
        register_class_once(instance).map_err(|error| format!("{error:#}"))
    });
    registration.as_ref().map(|_| ()).map_err(|error| anyhow!(error.clone()))
}

fn register_class_once(instance: HINSTANCE) -> Result<()> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class_name(),
        // SAFETY: The system owns the shared arrow cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.context("LoadCursorW(IDC_ARROW)")?,
        ..Default::default()
    };
    // SAFETY: `class` remains valid for the registration call.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(Error::from_thread()).context("RegisterClassExW for live logs");
    }
    Ok(())
}

fn run_message_loop() {
    let mut message = MSG::default();
    loop {
        // SAFETY: This thread owns the queue. `message` is writable storage.
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if result <= 0 {
            break;
        }
        // SAFETY: `GetMessageW` filled `message` with a valid queued message.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match catch_unwind(|| handle_message(hwnd, message, wparam, lparam)) {
        Ok(Some(result)) => result,
        Ok(None) | Err(_) => {
            // SAFETY: Win32 supplied all callback arguments.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

fn handle_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    _lparam: LPARAM,
) -> Option<LRESULT> {
    match message {
        WM_CREATE => Some(create_controls(hwnd)),
        WM_SIZE => {
            resize_editor(hwnd);
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == REFRESH_TIMER => {
            refresh_editor(hwnd);
            Some(LRESULT(0))
        }
        WM_FOCUS_VIEWER => {
            focus_window(hwnd);
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            // SAFETY: This callback owns `hwnd` on the viewer thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            // SAFETY: This callback owns the timer attached to `hwnd`.
            let _ = unsafe { KillTimer(Some(hwnd), REFRESH_TIMER) };
            // SAFETY: Only the dedicated viewer thread creates this class.
            // WM_QUIT therefore ends its own loop, never the application loop.
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        WM_NCDESTROY => {
            CLOSED.set(true);
            clear_generation(GENERATION.get());
            None
        }
        _ => None,
    }
}

fn create_controls(hwnd: HWND) -> LRESULT {
    let style = WS_CHILD
        | WS_VISIBLE
        | WS_VSCROLL
        | WS_HSCROLL
        | WINDOW_STYLE(
            (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_AUTOHSCROLL) as u32,
        );
    // SAFETY: `hwnd` is in WM_CREATE. Win32 copies the control strings.
    let editor = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            style,
            0,
            0,
            0,
            0,
            Some(hwnd),
            Some(HMENU(EDIT_ID as *mut c_void)),
            None,
            None,
        )
    };
    let Ok(editor) = editor else {
        return LRESULT(-1);
    };

    // SAFETY: The stock font is process-global. The edit control does not own it.
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    // SAFETY: `editor` is valid. WM_SETFONT reads the stock font handle only.
    unsafe {
        SendMessageW(
            editor,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    resize_editor(hwnd);
    // SAFETY: `hwnd` is valid, and this timer has no callback pointer.
    if unsafe { SetTimer(Some(hwnd), REFRESH_TIMER, REFRESH_MS, None) } == 0 {
        return LRESULT(-1);
    }
    LRESULT(0)
}

fn resize_editor(hwnd: HWND) {
    let mut area = RECT::default();
    // SAFETY: `hwnd` is live during its callback. `area` is writable storage.
    if unsafe { GetClientRect(hwnd, &mut area) }.is_err() {
        return;
    }
    // SAFETY: `hwnd` owns the edit child identified by `EDIT_ID`.
    let Ok(editor) = (unsafe { GetDlgItem(Some(hwnd), EDIT_ID) }) else {
        return;
    };
    let width = area.right.saturating_sub(area.left).max(0);
    let height = area.bottom.saturating_sub(area.top).max(0);
    let followed_tail = REVISION.get().is_some() && follows_tail(editor);
    // SAFETY: `editor` is valid. Dimensions are bounded by the client rectangle.
    let _ = unsafe { MoveWindow(editor, 0, 0, width, height, true) };
    if followed_tail {
        scroll_to_tail(editor);
    }
}

fn refresh_editor(hwnd: HWND) {
    // SAFETY: `hwnd` owns the edit child identified by `EDIT_ID`.
    let Ok(editor) = (unsafe { GetDlgItem(Some(hwnd), EDIT_ID) }) else {
        return;
    };
    if REVISION.get().is_some() && !follows_tail(editor) {
        return;
    }
    let snapshot = diagnostics::snapshot();
    if REVISION.get() == Some(snapshot.revision) {
        return;
    }
    let text = windows_text(&snapshot.text);
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and lives through the synchronous calls.
    unsafe {
        if SetWindowTextW(editor, PCWSTR(wide.as_ptr())).is_err() {
            return;
        }
        REVISION.set(Some(snapshot.revision));
        SendMessageW(
            editor,
            EM_SETSEL,
            Some(WPARAM(wide.len().saturating_sub(1))),
            Some(LPARAM(-1)),
        );
    }
    scroll_to_tail(editor);
}

fn scroll_to_tail(editor: HWND) {
    let mut scroll = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS,
        ..Default::default()
    };
    // SAFETY: The owning thread supplies a live edit control and writable storage.
    unsafe {
        SendMessageW(editor, EM_SCROLLCARET, None, None);
        if GetScrollInfo(editor, SB_HORZ, &mut scroll).is_ok() {
            TAIL_HORZ.set(scroll.nPos);
        }
    }
}

fn follows_tail(editor: HWND) -> bool {
    let (start, end) = selection(editor);
    if start != end {
        return false;
    }
    let mut scroll = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        ..Default::default()
    };
    // SAFETY: The UI thread owns `editor`. `scroll` is writable local storage.
    unsafe {
        if GetScrollInfo(editor, SB_HORZ, &mut scroll).is_ok() && scroll.nPos != TAIL_HORZ.get() {
            return false;
        }
        if GetScrollInfo(editor, SB_VERT, &mut scroll).is_err() {
            return true;
        }
    }
    let last = scroll.nMax.saturating_sub(
        i32::try_from(scroll.nPage.saturating_sub(1)).unwrap_or(i32::MAX));
    scroll.nPos >= last.max(scroll.nMin)
}

fn selection(editor: HWND) -> (u32, u32) {
    let mut start = 0u32;
    let mut end = 0u32;
    // SAFETY: EM_GETSEL writes two u32 values. Both live until the synchronous
    // SendMessageW call returns, including when native tests call across threads.
    unsafe {
        SendMessageW(editor, EM_GETSEL,
            Some(WPARAM((&mut start as *mut u32) as usize)),
            Some(LPARAM((&mut end as *mut u32) as isize)));
    }
    (start, end)
}

fn focus_window(hwnd: HWND) {
    // SAFETY: `hwnd` passed `IsWindow` before this message was queued.
    unsafe {
        let command = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
        let _ = ShowWindow(hwnd, command);
        let _ = SetForegroundWindow(hwnd);
        if let Ok(editor) = GetDlgItem(Some(hwnd), EDIT_ID) {
            let _ = SetFocus(Some(editor));
        }
    }
}

fn windows_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized.replace('\0', "\u{fffd}").replace('\n', "\r\n")
}

fn class_name() -> PCWSTR {
    w!("ChibipopLiveLogWindow")
}

fn hwnd_raw(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Controls::{EM_GETFIRSTVISIBLELINE, EM_LINESCROLL};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, IsWindow, IsWindowVisible,
        PeekMessageW, GWL_STYLE, PM_NOREMOVE, SB_LINERIGHT, WM_HSCROLL, WM_QUIT,
        WS_MAXIMIZEBOX, WS_SIZEBOX,
    };

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn viewer_reopens_and_closes_independently() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        diagnostics::clear_test_log();
        let first = show_ready(StartupHooks::default());
        // SAFETY: `first` is a live viewer handle.
        let style = unsafe { GetWindowLongW(first, GWL_STYLE) } as u32;
        assert_ne!(style & WS_SIZEBOX.0, 0);
        assert_ne!(style & WS_MAXIMIZEBOX.0, 0);

        diagnostics::append_test_log("viewer-live-日本語");
        assert!(wait_for_text(first, "viewer-live-日本語"));

        show().unwrap();
        assert_eq!(active_window(), Some(first));
        close_and_wait(first);

        diagnostics::append_test_log(&"large-history-日本語".repeat(5000));
        for _ in 0..12 {
            close_and_wait(show_ready(StartupHooks::default()));
        }
        let mut message = MSG::default();
        // SAFETY: This reads only the test thread's own queue without removal.
        assert!(!unsafe {
            PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_NOREMOVE)
        }.as_bool());
    }

    #[test]
    fn timed_out_startup_cannot_publish_over_a_replacement() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        diagnostics::clear_test_log();
        let (created_tx, created) = mpsc::channel();
        let (release, before_ready) = mpsc::channel();
        let (exited_tx, exited) = mpsc::channel();
        let result = show_with_timeout(Duration::from_millis(100), StartupHooks {
            created: Some(created_tx), before_ready: Some(before_ready),
            exited: Some(exited_tx), ..Default::default()
        });
        assert!(result.unwrap_err().to_string().contains("timed out"));
        let hidden = hwnd_from_raw(created.recv_timeout(Duration::from_secs(2)).unwrap());
        // SAFETY: The old UI thread waits at the test gate with its hidden window.
        assert!(!unsafe { IsWindowVisible(hidden) }.as_bool());
        assert!(active_window().is_none());
        let replacement = show_ready(StartupHooks::default());
        release.send(()).unwrap();
        exited.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(active_window(), Some(replacement));
        // SAFETY: The cancelled worker exited and destroyed its hidden window.
        assert!(!unsafe { IsWindow(Some(hidden)) }.as_bool());
        diagnostics::append_test_log("replacement-after-timeout\n");
        assert!(wait_for_text(replacement, "replacement-after-timeout"));
        close_and_wait(replacement);
    }

    #[test]
    fn old_thread_exit_cannot_clear_a_replacement_generation() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        diagnostics::clear_test_log();
        let (release, before_exit) = mpsc::channel();
        let (exited_tx, exited) = mpsc::channel();
        let first = show_ready(StartupHooks {
            before_exit: Some(before_exit), exited: Some(exited_tx), ..Default::default()
        });
        close_and_wait(first);
        let replacement = show_ready(StartupHooks::default());
        release.send(()).unwrap();
        exited.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(active_window(), Some(replacement));
        diagnostics::append_test_log("replacement-after-close\n");
        assert!(wait_for_text(replacement, "replacement-after-close"));
        close_and_wait(replacement);
    }

    #[test]
    fn refresh_pauses_for_selection_and_scroll_then_follows_tail() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        diagnostics::clear_test_log();
        diagnostics::append_test_log(&format!("{}\n", "horizontal-history".repeat(100)));
        diagnostics::append_test_log(&"selection-history-日本語\n".repeat(200));
        let hwnd = show_ready(StartupHooks::default());
        assert!(wait_for_text(hwnd, "selection-history-日本語"));
        // SAFETY: The live viewer owns this edit control.
        let editor = unsafe { GetDlgItem(Some(hwnd), EDIT_ID) }.unwrap();
        send(editor, EM_SETSEL, 5, 19);
        send(editor, EM_SCROLLCARET, 0, 0);
        let old_text = editor_text(editor);
        let old_top = send(editor, EM_GETFIRSTVISIBLELINE, 0, 0);
        diagnostics::append_test_log("pause-selection-new-tail\n");
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert_eq!(selection(editor), (5, 19));
        assert_eq!(send(editor, EM_GETFIRSTVISIBLELINE, 0, 0), old_top);
        assert_eq!(editor_text(editor), old_text);

        send(editor, EM_SETSEL, 0, 0);
        send(editor, EM_LINESCROLL, 0, -10000);
        diagnostics::append_test_log("pause-scroll-new-tail\n");
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert_eq!(send(editor, EM_GETFIRSTVISIBLELINE, 0, 0), 0);
        assert_eq!(editor_text(editor), old_text);

        let end = old_text.encode_utf16().count();
        send(editor, EM_SETSEL, end, -1);
        send(editor, EM_SCROLLCARET, 0, 0);
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert!(wait_for_text(hwnd, "pause-scroll-new-tail"));
        let latest_length = editor_text(editor).encode_utf16().count() as u32;
        assert_eq!(selection(editor), (latest_length, latest_length));

        let old_text = editor_text(editor);
        send(editor, WM_HSCROLL, SB_LINERIGHT.0 as usize, 0);
        diagnostics::append_test_log("pause-horizontal-new-tail\n");
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert_eq!(editor_text(editor), old_text);
        send(editor, EM_SCROLLCARET, 0, 0);
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert!(wait_for_text(hwnd, "pause-horizontal-new-tail"));

        let long_line = format!("long-unfinished-line:{}", "日本語".repeat(400));
        diagnostics::append_test_log(&long_line);
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert!(wait_for_text(hwnd, &long_line));
        diagnostics::append_test_log("-live-tail-after-long-line");
        send(hwnd, WM_TIMER, REFRESH_TIMER, 0);
        assert!(wait_for_text(hwnd, "-live-tail-after-long-line"));
        close_and_wait(hwnd);
    }

    fn send(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
        // SAFETY: Tests supply a live handle and documented message parameters.
        unsafe { SendMessageW(hwnd, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))) }.0
    }

    fn editor_text(editor: HWND) -> String {
        // SAFETY: The control remains live. The buffer permits its text and NUL.
        unsafe {
            let length = GetWindowTextLengthW(editor);
            let mut text = vec![0u16; length.max(0) as usize + 1];
            let count = GetWindowTextW(editor, &mut text);
            String::from_utf16_lossy(&text[..count.max(0) as usize])
        }
    }

    fn wait_for_window() -> HWND {
        for _ in 0..100 {
            if let Some(hwnd) = active_window() {
                return hwnd;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("viewer did not open");
    }

    fn show_ready(mut hooks: StartupHooks) -> HWND {
        let (ready_tx, ready) = mpsc::channel();
        hooks.ready = Some(ready_tx);
        show_with_timeout(START_TIMEOUT, hooks).unwrap();
        ready.recv_timeout(START_TIMEOUT).expect("viewer did not finish initial rendering");
        wait_for_window()
    }

    fn close_and_wait(hwnd: HWND) {
        // SAFETY: `hwnd` is a live viewer handle.
        unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
        for _ in 0..100 {
            if active_window().is_none() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let state = VIEWER.lock().unwrap().as_ref()
            .map(|viewer| (viewer.generation, viewer.hwnd));
        panic!("viewer did not close: expected={} active={state:?}", hwnd_raw(hwnd));
    }

    fn wait_for_text(hwnd: HWND, expected: &str) -> bool {
        for _ in 0..100 {
            // SAFETY: `hwnd` owns the edit child identified by `EDIT_ID`.
            let editor = unsafe { GetDlgItem(Some(hwnd), EDIT_ID) }.unwrap();
            // SAFETY: `editor` remains live while the viewer window is open.
            let length = unsafe { GetWindowTextLengthW(editor) };
            let mut text = vec![0u16; usize::try_from(length).unwrap_or(0).saturating_add(1)];
            // SAFETY: `text` has room for the reported content and trailing NUL.
            let count = unsafe { GetWindowTextW(editor, &mut text) };
            if String::from_utf16_lossy(&text[..usize::try_from(count).unwrap_or(0)])
                .contains(expected)
            {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}
