//! A native EDIT owns IME composition. The main pump submits committed text
//! and presents replies; the database thread never calls a window API.

use anyhow::{Context, Result};
use chibipop::config::Config;
use chibipop::search::{result_text, SearchResult, SearchService};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetStockObject, GetSysColorBrush, COLOR_WINDOW, DEFAULT_GUI_FONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

const SUBMIT: usize = 101;
const WM_IME_START: u32 = 0x010D;
const WM_IME_END: u32 = 0x010E;
const CLASS: PCWSTR = w!("ChibipopSearchWindow");

struct State {
    input: Cell<HWND>,
    results: Cell<HWND>,
    button: Cell<HWND>,
    composing: Cell<bool>,
    submit: Cell<bool>,
}

struct Query { generation: u64, text: String, config: Config }

pub struct SearchWindow {
    hwnd: HWND,
    state: Box<State>,
    config: Config,
    config_path: Option<PathBuf>,
    generation: u64,
    requests: Sender<Query>,
    replies: Receiver<(u64, String)>,
}

fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() }

pub fn is_foreground() -> bool {
    let mut name = [0u16; 64];
    // SAFETY: The buffer is live. These queries do not retain pointers.
    let count = unsafe { GetClassNameW(GetForegroundWindow(), &mut name) };
    count > 0 && "ChibipopSearchWindow".encode_utf16().eq(name[..count as usize].iter().copied())
}

impl SearchWindow {
    pub fn open(database: &Path, rules: &Path, config: &Config) -> Result<Self> {
        let state = Box::new(State {
            input: Cell::new(HWND::default()), results: Cell::new(HWND::default()),
            button: Cell::new(HWND::default()), composing: Cell::new(false), submit: Cell::new(false),
        });
        // SAFETY: The class callback remains valid. State stays allocated until after DestroyWindow.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW)?, hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(Error::from_thread()).context("registering the search window");
            }
            CreateWindowExW(WS_EX_CONTROLPARENT, CLASS, w!("chibipop search"), WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT, CW_USEDEFAULT, 760, 640, None, None, Some(instance.into()),
                Some(state.as_ref() as *const State as *const std::ffi::c_void))?
        };
        let (requests, inbox) = mpsc::channel::<Query>();
        let (outbox, replies) = mpsc::channel();
        let database = database.to_path_buf();
        let rules = rules.to_path_buf();
        if let Err(error) = std::thread::Builder::new().name("dictionary-search".into()).spawn(move || {
            while let Ok(mut query) = inbox.recv() {
                while let Ok(newer) = inbox.try_recv() { query = newer; }
                let text = search_text(&database, &rules, &query);
                if outbox.send((query.generation, text)).is_err() { break; }
            }
        }) {
            // SAFETY: This thread created hwnd and still owns its state.
            unsafe { let _ = DestroyWindow(hwnd); }
            return Err(error).context("starting dictionary search");
        }
        let window = Self { hwnd, state, config: config.clone(), config_path: None, generation: 0, requests, replies };
        window.show();
        Ok(window)
    }

    pub fn show(&self) {
        self.state.submit.set(true);
        // SAFETY: All handles belong to this window and its creating thread.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.state.input.get()));
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: hwnd remains live until Drop.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn handle_message(&self, message: &MSG) -> bool {
        let controls = [self.state.input.get(), self.state.button.get(), self.state.results.get()];
        let Some(index) = controls.iter().position(|hwnd| *hwnd == message.hwnd) else { return false };
        if message.message != WM_KEYDOWN || self.state.composing.get() { return false; }
        // SAFETY: The controls belong to this thread. Queries and focus changes retain no pointers.
        unsafe {
            match message.wParam.0 {
                9 => {
                    let next = if GetKeyState(0x10) < 0 { (index + 2) % 3 } else { (index + 1) % 3 };
                    let _ = SetFocus(Some(controls[next]));
                    true
                }
                27 => { let _ = ShowWindow(self.hwnd, SW_HIDE); true }
                13 if message.hwnd == self.state.button.get() => { self.state.submit.set(true); true }
                _ => false,
            }
        }
    }

    pub fn update_config(&mut self, config: &Config) {
        self.config = config.clone();
        self.generation = self.generation.wrapping_add(1);
        self.state.submit.set(true);
    }

    pub fn set_config_path(&mut self, path: &Path) {
        self.config_path = Some(path.to_path_buf());
    }

    pub fn poll(&mut self) {
        if self.state.submit.replace(false) {
            self.generation = self.generation.wrapping_add(1);
            if let Some(path) = &self.config_path {
                match chibipop::config::load_or_create(path) {
                    Ok(config) => self.config = config,
                    Err(error) => {
                        self.set_results(&format!("Cannot load search settings: {error:#}"));
                        return;
                    }
                }
            }
            let text = read_text(self.state.input.get());
            if text.trim().is_empty() {
                self.set_results(&result_text(&SearchResult::Empty));
            } else {
                self.set_results("Searching…");
                if self.requests.send(Query { generation: self.generation, text,
                    config: self.config.clone() }).is_err() {
                    self.set_results("Search stopped. Close and reopen the application to retry.");
                }
            }
        }
        while let Ok((generation, text)) = self.replies.try_recv() {
            if generation == self.generation { self.set_results(&text); }
        }
    }

    fn set_results(&self, text: &str) {
        let text = wide(&text.replace('\n', "\r\n"));
        // SAFETY: SetWindowTextW copies this terminated buffer before returning.
        unsafe { let _ = SetWindowTextW(self.state.results.get(), PCWSTR(text.as_ptr())); }
    }
}

impl Drop for SearchWindow {
    fn drop(&mut self) {
        // SAFETY: The window belongs to this thread. State outlives this call.
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}

fn search_text(database: &Path, rules: &Path, query: &Query) -> String {
    match SearchService::open(database, rules, &query.config).and_then(|service| service.search(&query.text)) {
        Ok(result) => result_text(&result),
        Err(error) => format!("Search failed: {error:#}\nCheck the dictionaries in Settings, then search again."),
    }
}

fn read_text(hwnd: HWND) -> String {
    // SAFETY: The edit is live, and GetWindowTextW respects the buffer length.
    unsafe {
        let mut text = vec![0u16; (GetWindowTextLengthW(hwnd).max(0) as usize) + 1];
        let length = GetWindowTextW(hwnd, &mut text);
        String::from_utf16_lossy(&text[..length.max(0) as usize])
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: Win32 owns message arguments. GWLP_USERDATA points to the live State supplied at creation.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
        if pointer.is_null() { return DefWindowProcW(hwnd, msg, wp, lp); }
        let state = &*pointer;
        match msg {
            WM_CREATE => {
                let create = |class, text, style, id| CreateWindowExW(WINDOW_EX_STYLE(0), class, text,
                    WS_CHILD | WS_VISIBLE | style, 12, 12, 100, 28, Some(hwnd),
                    Some(HMENU(id as *mut std::ffi::c_void)), None, None);
                let input = create(w!("EDIT"), w!(""), WS_BORDER | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32), 100);
                let button = create(w!("BUTTON"), w!("Search"), WS_TABSTOP, SUBMIT);
                let results = create(w!("EDIT"), w!("Type a Japanese word or expression, then press Enter."),
                    WS_BORDER | WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32), 102);
                let (Ok(input), Ok(button), Ok(results)) = (input, button, results) else { return LRESULT(-1) };
                state.input.set(input); state.button.set(button); state.results.set(results);
                let font = GetStockObject(DEFAULT_GUI_FONT);
                for control in [input, button, results] {
                    SendMessageW(control, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
                }
                if !SetWindowSubclass(input, Some(input_proc), 1, pointer as usize).as_bool() { return LRESULT(-1); }
                SendMessageW(input, EM_SETLIMITTEXT, Some(WPARAM(4096)), None);
                SendMessageW(results, EM_SETLIMITTEXT, Some(WPARAM(0x7ffffffe)), None);
                LRESULT(0)
            }
            WM_SIZE => {
                let width = (lp.0 as u32 & 0xffff) as i32;
                let height = ((lp.0 as u32 >> 16) & 0xffff) as i32;
                let _ = MoveWindow(state.input.get(), 12, 12, (width - 124).max(40), 30, true);
                let _ = MoveWindow(state.button.get(), (width - 100).max(52), 12, 88, 30, true);
                let _ = MoveWindow(state.results.get(), 12, 54, (width - 24).max(40), (height - 66).max(30), true);
                LRESULT(0)
            }
            WM_COMMAND if wp.0 & 0xffff == SUBMIT && !state.composing.get() => {
                state.submit.set(true); LRESULT(0)
            }
            WM_ACTIVATE if wp.0 & 0xffff != WA_INACTIVE as usize => {
                crate::input::hooks::clear_keyboard_actions();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SETFOCUS => { let _ = SetFocus(Some(state.input.get())); LRESULT(0) }
            WM_CLOSE => { let _ = ShowWindow(hwnd, SW_HIDE); LRESULT(0) }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn input_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM,
    _id: usize, data: usize) -> LRESULT {
    // SAFETY: The parent owns State until its children are destroyed. DefSubclassProc receives unchanged arguments.
    unsafe {
        let state = &*(data as *const State);
        match msg {
            WM_IME_START => state.composing.set(true),
            WM_IME_END => state.composing.set(false),
            WM_KEYDOWN if wp.0 == 13 && !state.composing.get() => {
                state.submit.set(true); return LRESULT(0);
            }
            WM_CHAR if wp.0 == 13 && !state.composing.get() => return LRESULT(0),
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
    }

    fn fixture() -> (SearchWindow, Fixture, impl Sized) {
        let guard = crate::input::hooks::search_keyboard_test_guard();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let database = std::env::temp_dir().join(format!("chibipop-native-search-{}-{}.sqlite",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let fixture = Fixture(database.clone());
        chibipop::dict::build::build(&[root.join("tests/fixtures/yomitan/terms.zip")],
            &[], &database, &|_| {}).unwrap();
        let window = SearchWindow::open(&database, &root.join("data/deconjugator.json"), &Config::default()).unwrap();
        (window, fixture, guard)
    }

    fn input(window: &SearchWindow, text: &str) {
        let text = wide(text);
        // SAFETY: The input and terminated buffer remain live throughout these synchronous calls.
        unsafe {
            SetWindowTextW(window.state.input.get(), PCWSTR(text.as_ptr())).unwrap();
            SendMessageW(window.state.input.get(), WM_KEYDOWN, Some(WPARAM(13)), None);
        }
    }

    fn wait_result(window: &mut SearchWindow, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let mut message = MSG::default();
            // SAFETY: The current thread owns the message storage and the test windows.
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    if !window.handle_message(&message) {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            window.poll();
            let actual = read_text(window.state.results.get());
            if actual.contains(expected) { return; }
            assert!(Instant::now() < deadline, "expected {expected:?}, got {actual:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn native_edit_submit_empty_miss_reconfigure_and_reopen() {
        let (mut window, fixture_db, _guard) = fixture();
        input(&window, "食べました");
        wait_result(&mut window, "to eat");
        assert!(read_text(window.state.results.get()).contains("たべる"));
        input(&window, "絶対にない検索");
        wait_result(&mut window, "No matching entries");
        input(&window, "　 ");
        wait_result(&mut window, "Type a Japanese word");
        input(&window, "猫");
        wait_result(&mut window, "cat (kanji)");
        let mut config = Config::default();
        config.dictionaries.terms_disabled = vec!["FixtureTerms".into()];
        window.update_config(&config);
        wait_result(&mut window, "No matching entries");
        // SAFETY: The current thread owns this test window.
        unsafe { SendMessageW(window.hwnd, WM_CLOSE, None, None); }
        assert!(!window.is_visible());
        config.dictionaries.terms_disabled.clear();
        window.update_config(&config);
        window.show();
        assert!(window.is_visible());
        wait_result(&mut window, "cat (kanji)");
        assert_eq!(read_text(window.state.input.get()), "猫");
        let connection = rusqlite::Connection::open(&fixture_db.0).unwrap();
        connection.execute("DROP TABLE term", []).unwrap();
        input(&window, "猫");
        wait_result(&mut window, "Search failed:");
    }

    #[test]
    fn native_stale_reply_cannot_replace_current_results() {
        let (mut window, _fixture, _guard) = fixture();
        window.state.submit.set(false);
        window.generation = 7;
        let (sender, replies) = mpsc::channel();
        window.replies = replies;
        window.set_results("original");
        sender.send((6, "stale".into())).unwrap();
        window.poll();
        assert_eq!(read_text(window.state.results.get()), "original");
        sender.send((7, "current".into())).unwrap();
        window.poll();
        assert_eq!(read_text(window.state.results.get()), "current");
        window.update_config(&Config::default());
        window.state.submit.set(false);
        sender.send((7, "obsolete config".into())).unwrap();
        window.poll();
        assert_eq!(read_text(window.state.results.get()), "current");
    }

    #[test]
    fn native_ime_enter_and_modeless_keyboard_routing() {
        let (mut window, _fixture, _guard) = fixture();
        window.state.submit.set(false);
        let edit = window.state.input.get();
        // SAFETY: These synchronous messages target the test's live edit control.
        unsafe {
            SendMessageW(edit, WM_IME_START, None, None);
            SendMessageW(edit, WM_KEYDOWN, Some(WPARAM(13)), None);
        }
        assert!(!window.state.submit.get());
        let enter = MSG { hwnd: edit, message: WM_KEYDOWN, wParam: WPARAM(13), ..Default::default() };
        assert!(!window.handle_message(&enter));
        let escape = MSG { wParam: WPARAM(27), ..enter };
        assert!(!window.handle_message(&escape));
        assert!(window.is_visible());
        // SAFETY: The edit belongs to this thread. No physical keyboard input is sent.
        unsafe {
            SendMessageW(edit, WM_IME_END, None, None);
            SendMessageW(edit, WM_KEYDOWN, Some(WPARAM(13)), None);
        }
        assert!(window.state.submit.get());
        window.poll();
        let tab = MSG { wParam: WPARAM(9), ..enter };
        struct KeyboardState([u8; 256]);
        impl Drop for KeyboardState {
            fn drop(&mut self) {
                // SAFETY: The saved keyboard state belongs to this test thread.
                unsafe { let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&self.0); }
            }
        }
        let mut keys = [0u8; 256];
        // SAFETY: GetKeyboardState fills this live, fixed-size buffer.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState(&mut keys).unwrap(); }
        let _keyboard = KeyboardState(keys);
        keys[0x10] = 0;
        // SAFETY: This changes only the current test thread's key state, not physical input.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&keys).unwrap(); }
        assert!(window.handle_message(&tab));
        // SAFETY: GetFocus only queries this thread's focus window.
        assert_eq!(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() }, window.state.button.get());
        keys[0x10] = 0x80;
        // SAFETY: This changes only the current test thread's key state, not physical input.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&keys).unwrap(); }
        let back_tab = MSG { hwnd: window.state.button.get(), ..tab };
        assert!(window.handle_message(&back_tab));
        // SAFETY: GetFocus only queries this thread's focus window.
        assert_eq!(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() }, edit);
        let foreign = MSG { hwnd: HWND::default(), ..tab };
        assert!(!window.handle_message(&foreign));
        assert!(window.handle_message(&escape));
        assert!(!window.is_visible());
        window.show();
        assert!(window.is_visible());
    }
}
