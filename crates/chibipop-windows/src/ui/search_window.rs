//! Native edits own composition. Workers own lookup and sentence analysis.
//! The UI thread retains all HWNDs, GDI resources, and definition renderers.

use super::search_popup::{self, Action, SearchPopup};
use super::theme::Theme;
use anyhow::{Context, Result};
use chibipop::config::Config;
use chibipop::geom::PhysPoint;
use chibipop::search::{candidates, selected_presentation, SearchMode, SearchResult,
    SearchService, SentenceAnalyzer, SentenceToken};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

const INPUT: usize = 100;
const SUBMIT: usize = 101;
const RESULTS: usize = 102;
const SENTENCE: usize = 103;
const MODE: usize = 104;
const STATUS: usize = 105;
const INPUT_LABEL: usize = 106;
const SENTENCE_LABEL: usize = 107;
const RESULTS_LABEL: usize = 108;
const WM_IME_START: u32 = 0x010D;
const WM_IME_END: u32 = 0x010E;
const CLASS: PCWSTR = w!("ChibipopSearchWindow");

struct Palette {
    theme: Theme,
    background: HBRUSH,
    accent: HBRUSH,
    border: HBRUSH,
    border_pen: HPEN,
    body_font: HFONT,
    headword_font: HFONT,
    reading_font: HFONT,
    summary_font: HFONT,
    dimmed_font: HFONT,
    sentence_font: HFONT,
    scale: f32,
}

impl Palette {
    fn new(config: &Config, scale: f32) -> Result<Self> {
        let theme = search_popup::controls_theme(config);
        let face = wide(&theme.font_name);
        let sentence_size = (theme.body_size * 1.35)
            .max(theme.headword_size + 2.0)
            .max(theme.collapsed_size + 2.0);
        let border_width = (theme.border_width * scale).round().max(1.0) as i32;
        // SAFETY: GDI copies parameters. Returned handles have one Palette owner.
        unsafe {
            let background = CreateSolidBrush(color(theme.background));
            let accent = CreateSolidBrush(color(theme.accent));
            let border = CreateSolidBrush(color(theme.border));
            let border_pen = CreatePen(PS_SOLID, border_width, color(theme.border));
            let body_font = create_font(&face, theme.body_size, theme.body_weight,
                theme.body_italic, scale);
            let headword_font = create_font(&face, theme.headword_size, theme.headword_weight,
                theme.headword_italic, scale);
            let reading_font = create_font(&face, theme.reading_size, theme.reading_weight,
                theme.reading_italic, scale);
            let summary_font = create_font(&face, theme.collapsed_size, theme.collapsed_weight,
                theme.collapsed_italic, scale);
            let dimmed_font = create_font(&face, theme.dimmed_size, theme.dimmed_weight,
                theme.dimmed_italic, scale);
            let sentence_font = create_font(&face, sentence_size, theme.body_weight,
                theme.body_italic, scale);
            let palette = Self { theme, background, accent, border, border_pen, body_font,
                headword_font, reading_font, summary_font, dimmed_font, sentence_font, scale };
            if background.is_invalid() || accent.is_invalid() || border.is_invalid()
                || border_pen.is_invalid() || body_font.is_invalid() || headword_font.is_invalid()
                || reading_font.is_invalid() || summary_font.is_invalid()
                || dimmed_font.is_invalid() || sentence_font.is_invalid() {
                anyhow::bail!("creating search theme resources");
            }
            Ok(palette)
        }
    }
}

impl Drop for Palette {
    fn drop(&mut self) {
        // SAFETY: Controls receive replacement fonts before old objects are freed.
        unsafe {
            for font in [self.body_font, self.headword_font, self.reading_font,
                self.summary_font, self.dimmed_font, self.sentence_font] {
                let _ = DeleteObject(font.into());
            }
            let _ = DeleteObject(self.background.into());
            let _ = DeleteObject(self.accent.into());
            let _ = DeleteObject(self.border.into());
            let _ = DeleteObject(self.border_pen.into());
        }
    }
}

struct State {
    input: Cell<HWND>, results: Cell<HWND>, button: Cell<HWND>, sentence: Cell<HWND>,
    status: Cell<HWND>, mode_button: Cell<HWND>, input_label: Cell<HWND>,
    sentence_label: Cell<HWND>, results_label: Cell<HWND>, mode: Cell<SearchMode>,
    composing: Cell<bool>, submit: Cell<bool>, switch: Cell<bool>, dpi_changed: Cell<bool>,
    dismissed: Cell<bool>, preserve_definitions: Cell<bool>,
    clicked: Cell<Option<usize>>, selected: Cell<Option<usize>>,
    palette: RefCell<Palette>,
}

struct Query {
    generation: u64, text: String, config: Config, mode: SearchMode,
    clicked: Option<usize>, definition: Option<(usize, bool)>,
}

struct Reply {
    generation: u64, result: std::result::Result<SearchResult, String>,
    tokens: Vec<SentenceToken>, selected: Option<Range<usize>>, definition: Option<(usize, bool)>,
}

pub struct SearchWindow {
    hwnd: HWND,
    state: Box<State>,
    config: Config,
    config_path: Option<PathBuf>,
    database: PathBuf,
    generation: u64,
    requests: Sender<Query>,
    replies: Receiver<Reply>,
    result: SearchResult,
    tokens: Vec<SentenceToken>,
    definitions: Vec<SearchPopup>,
    hover: Option<(usize, String, Instant, bool)>,
}

fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() }
fn color((r, g, b): (u8, u8, u8)) -> COLORREF { COLORREF(u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16) }

unsafe fn create_font(face: &[u16], size: f32, weight: u16, italic: bool, scale: f32) -> HFONT {
    // SAFETY: The caller keeps the terminated face buffer live through this call.
    unsafe {
        CreateFontW(-((size * scale).round() as i32).max(1), 0, 0, 0, i32::from(weight),
            u32::from(italic), 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY, DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()))
    }
}

pub fn is_foreground() -> bool {
    // SAFETY: Owner and class queries retain no pointers.
    unsafe {
        let mut hwnd = GetForegroundWindow();
        for _ in 0..16 {
            let mut name = [0u16; 64];
            let count = GetClassNameW(hwnd, &mut name);
            if count > 0
                && "ChibipopSearchWindow".encode_utf16()
                    .eq(name[..count as usize].iter().copied()) {
                return true;
            }
            let Ok(owner) = GetWindow(hwnd, GW_OWNER) else { return false };
            if owner.is_invalid() || owner == hwnd { return false; }
            hwnd = owner;
        }
        false
    }
}

impl SearchWindow {
    pub fn open(database: &Path, rules: &Path, config: &Config) -> Result<Self> {
        Self::open_mode(database, rules, config, SearchMode::Dictionary, None)
    }

    pub fn open_mode(database: &Path, rules: &Path, config: &Config,
        mode: SearchMode, text: Option<&str>) -> Result<Self> {
        let state = Box::new(State {
            input: Cell::default(), results: Cell::default(), button: Cell::default(),
            sentence: Cell::default(), status: Cell::default(), mode_button: Cell::default(),
            input_label: Cell::default(), sentence_label: Cell::default(),
            results_label: Cell::default(),
            mode: Cell::new(mode), composing: Cell::new(false), submit: Cell::new(false),
            switch: Cell::new(false), dpi_changed: Cell::new(false), dismissed: Cell::new(false),
            preserve_definitions: Cell::new(false), clicked: Cell::new(None),
            selected: Cell::new(None),
            palette: RefCell::new(Palette::new(config, 1.0)?),
        });
        // SAFETY: The stable State allocation outlives every native callback.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(),
                lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
            if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(Error::from_thread()).context("registering search window");
            }
            CreateWindowExW(WS_EX_CONTROLPARENT | WS_EX_LAYERED, CLASS, w!("Dictionary search"), WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT, CW_USEDEFAULT, 760, 680, None, None, Some(instance.into()),
                Some(state.as_ref() as *const State as *const std::ffi::c_void))?
        };
        // SAFETY: This timer only posts messages to this thread's HWND and dies with that HWND.
        unsafe {
            if SetTimer(Some(hwnd), 1, 20, None) == 0 {
                let _ = DestroyWindow(hwnd);
                return Err(Error::from_thread()).context("starting search window wake timer");
            }
        }
        let (requests, inbox) = mpsc::channel::<Query>();
        let (outbox, replies) = mpsc::channel();
        let db = database.to_path_buf();
        let rules = rules.to_path_buf();
        if let Err(error) = std::thread::Builder::new().name("dictionary-search".into()).spawn(move || {
            let mut analyzer = SentenceAnalyzer::new(crate::paths::data_file(chibipop::analysis::MODEL_FILE));
            while let Ok(mut query) = inbox.recv() {
                while let Ok(newer) = inbox.try_recv() { query = newer; }
                let reply = search(&db, &rules, &query, &mut analyzer);
                if outbox.send(reply).is_err() { break; }
            }
        }) {
            // SAFETY: This thread owns HWND and State remains live during destruction.
            unsafe { let _ = DestroyWindow(hwnd); }
            return Err(error).context("starting search worker");
        }
        let mut window = Self { hwnd, state, config: config.clone(), config_path: None,
            database: database.to_path_buf(), generation: 0, requests, replies,
            result: SearchResult::Empty, tokens: Vec::new(), definitions: Vec::new(), hover: None };
        window.update_config(config);
        window.switch_mode(mode, text);
        Ok(window)
    }

    pub fn switch_mode(&mut self, mode: SearchMode, text: Option<&str>) {
        self.invalidate();
        self.state.preserve_definitions.set(false);
        self.state.mode.set(mode);
        self.state.clicked.set(None);
        let title = if mode == SearchMode::Dictionary { "Dictionary search" } else { "Sentence search" };
        let button = if mode == SearchMode::Dictionary { "Sentence search" } else { "Dictionary search" };
        let input_label = if mode == SearchMode::Dictionary { "Search term" } else { "Japanese sentence" };
        let submit = if mode == SearchMode::Dictionary { "Search dictionary" } else { "Analyze sentence" };
        let results_label = if mode == SearchMode::Dictionary { "Dictionary candidates" }
            else { "Candidates for selected word" };
        // SAFETY: These setters copy strings on the HWND owner thread.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide(title).as_ptr()));
            let _ = SetWindowTextW(self.state.mode_button.get(), PCWSTR(wide(button).as_ptr()));
            let _ = SetWindowTextW(self.state.input_label.get(), PCWSTR(wide(input_label).as_ptr()));
            let _ = SetWindowTextW(self.state.button.get(), PCWSTR(wide(submit).as_ptr()));
            let _ = SetWindowTextW(self.state.results_label.get(), PCWSTR(wide(results_label).as_ptr()));
            if let Some(text) = text { let _ = SetWindowTextW(self.state.input.get(), PCWSTR(wide(text).as_ptr())); }
            apply_fonts(&self.state);
            layout(self.hwnd, &self.state);
        }
        self.show();
    }

    pub fn show(&self) {
        self.state.dismissed.set(false);
        self.state.submit.set(true);
        // SAFETY: The HWNDs remain owned by this thread until Drop.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.state.input.get()));
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: The window remains live until Drop.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn handle_message(&mut self, message: &MSG) -> bool {
        let controls = [self.state.input.get(), self.state.button.get(), self.state.mode_button.get(),
            self.state.sentence.get(), self.state.results.get()];
        let index = controls.iter().position(|hwnd| *hwnd == message.hwnd);
        let definition = self.definitions.iter()
            .position(|popup| popup.owns_window(message.hwnd));
        let owned = message.hwnd == self.hwnd || index.is_some() || definition.is_some();
        if !owned || message.message != WM_KEYDOWN || self.state.composing.get() { return false; }
        if message.wParam.0 == 27 {
            if message.lParam.0 & (1 << 30) != 0 { return true; }
            if let Some(definition) = definition {
                self.cancel_pending();
                self.definitions.truncate(definition);
                self.restore_focus();
            } else {
                self.dismiss();
            }
            return true;
        }
        let Some(index) = index else { return false };
        // SAFETY: Focus changes target this thread's controls only.
        unsafe {
            match message.wParam.0 {
                9 => {
                    let backward = GetKeyState(0x10) < 0;
                    for step in 1..=controls.len() {
                        let next = if backward { (index + controls.len() - step) % controls.len() }
                            else { (index + step) % controls.len() };
                        if IsWindowVisible(controls[next]).as_bool() { let _ = SetFocus(Some(controls[next])); break; }
                    }
                    true
                }
                13 if message.hwnd == self.state.results.get() => {
                    let index = SendMessageW(message.hwnd, LB_GETCURSEL, None, None).0;
                    self.state.selected.set(usize::try_from(index).ok()); true
                }
                13 if message.hwnd == self.state.mode_button.get() => { self.state.switch.set(true); true }
                13 if message.hwnd == self.state.button.get() => { self.state.submit.set(true); true }
                _ => false,
            }
        }
    }

    pub fn update_config(&mut self, config: &Config) {
        self.config = config.clone();
        self.cancel_pending();
        self.state.preserve_definitions.set(true);
        // SAFETY: The DPI query reads this thread's live window.
        let scale = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
        match Palette::new(config, scale) {
            Ok(palette) => {
                // SAFETY: Controls switch fonts before old GDI objects drop.
                unsafe {
                    let previous = self.state.palette.replace(palette);
                    apply_fonts(&self.state);
                    layout(self.hwnd, &self.state);
                    drop(previous);
                    let _ = InvalidateRect(Some(self.hwnd), None, true);
                }
            }
            Err(error) => self.set_status(&format!("Cannot apply search theme: {error:#}")),
        }
        let mut definition_error = None;
        for popup in &mut self.definitions {
            if let Err(error) = popup.update_config(config) {
                definition_error = Some(error);
                break;
            }
        }
        if let Some(error) = definition_error {
            self.set_status(&format!("Cannot apply definition theme: {error:#}"));
        }
        self.state.submit.set(true);
    }

    pub fn set_config_path(&mut self, path: &Path) { self.config_path = Some(path.to_path_buf()); }

    fn invalidate(&mut self) {
        self.cancel_pending();
        self.definitions.clear();
    }

    fn cancel_pending(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.hover = None;
        self.state.selected.set(None);
    }

    fn restore_focus(&self) {
        if let Some(popup) = self.definitions.last() { popup.activate(); }
        else {
            // SAFETY: This thread owns the visible search and input control.
            unsafe { let _ = SetForegroundWindow(self.hwnd); let _ = SetFocus(Some(self.state.input.get())); }
        }
    }

    fn dismiss(&self) {
        self.state.dismissed.set(true);
        self.state.submit.set(false);
        self.state.switch.set(false);
        self.state.clicked.set(None);
        self.state.selected.set(None);
        for popup in &self.definitions { popup.hide(); }
        // SAFETY: This thread owns the live search HWND.
        unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); }
    }

    pub fn poll(&mut self) {
        if self.state.dismissed.replace(false) || !self.is_visible() {
            self.invalidate();
            self.state.preserve_definitions.set(false);
            return;
        }
        if self.state.dpi_changed.replace(false) { self.update_config(&self.config.clone()); }
        if self.state.switch.replace(false) {
            let mode = if self.state.mode.get() == SearchMode::Dictionary { SearchMode::Sentence } else { SearchMode::Dictionary };
            self.switch_mode(mode, None);
        }
        if let Some(index) = self.definitions.iter().position(SearchPopup::close_requested) {
            self.cancel_pending();
            self.definitions.truncate(index);
            self.restore_focus();
            return;
        }
        if !self.state.composing.get() && self.state.submit.replace(false) {
            if self.state.preserve_definitions.replace(false) { self.cancel_pending(); }
            else { self.invalidate(); }
            if let Some(path) = &self.config_path {
                match chibipop::config::load_or_create(path) {
                    Ok(config) if config != self.config => {
                        self.update_config(&config); self.state.submit.set(false);
                    }
                    Ok(_) => {}
                    Err(error) => { self.set_status(&format!("Cannot load search settings: {error:#}")); return; }
                }
            }
            let text = read_text(self.state.input.get());
            if self.state.mode.get() == SearchMode::Sentence {
                // SAFETY: The view copies the current input synchronously.
                unsafe { let _ = SetWindowTextW(self.state.sentence.get(), PCWSTR(wide(&text).as_ptr())); }
            }
            self.clear_results(); self.set_status("Searching…");
            self.enqueue(text, self.state.mode.get(), self.state.clicked.take(), None);
        } else if !self.state.composing.get() {
            if let Some(offset) = self.state.clicked.take() {
                self.invalidate(); self.clear_results();
                self.enqueue(read_text(self.state.input.get()), SearchMode::Sentence, Some(offset), None);
            }
        }
        while let Ok(reply) = self.replies.try_recv() {
            if reply.generation != self.generation || self.state.composing.get() { continue; }
            if let Some((parent, hover)) = reply.definition {
                if hover {
                    let expected = self.hover.as_ref().filter(|(index, _, _, sent)| *index == parent && *sent)
                        .map(|(_, query, _, _)| query.clone());
                    let current = self.definitions.get_mut(parent).filter(|popup| popup.contains_pointer())
                        .and_then(SearchPopup::hover);
                    if expected.is_none() || expected != current { continue; }
                }
                if let Ok(SearchResult::Found(presentation)) = reply.result {
                    if let Some(popup) = self.definitions.get(parent) {
                        if let Ok(anchor) = popup.child_anchor() {
                            self.definitions.truncate(parent + 1);
                            match SearchPopup::open(&self.database, &self.config, self.hwnd, *presentation, anchor, true) {
                                Ok(popup) => self.definitions.push(popup),
                                Err(error) => self.set_status(&format!("Cannot open definition: {error:#}")),
                            }
                        }
                    }
                }
                continue;
            }
            self.tokens = reply.tokens;
            if let Some(range) = reply.selected { self.highlight(range); }
            else if self.state.mode.get() == SearchMode::Sentence { self.highlight(0..0); }
            match reply.result {
                Ok(result) => { self.result = result; self.render_candidates(); }
                Err(error) => { self.clear_results(); self.set_status(&format!("Search failed: {error}")); }
            }
        }
        if let Some(index) = self.state.selected.take() {
            if let Some(presentation) = selected_presentation(&self.result, index) {
                self.generation = self.generation.wrapping_add(1);
                self.definitions.clear(); self.hover = None;
                let mut point = POINT { x: 24, y: 48 };
                // SAFETY: Conversion uses the owned listbox HWND and point storage.
                unsafe { let _ = ClientToScreen(self.state.results.get(), &mut point); }
                match SearchPopup::open(&self.database, &self.config, self.hwnd, presentation,
                    PhysPoint { x: point.x, y: point.y }, false) {
                    Ok(popup) => self.definitions.push(popup),
                    Err(error) => self.set_status(&format!("Cannot open definition: {error:#}")),
                }
            }
        }
        self.poll_definitions();
    }

    fn enqueue(&self, text: String, mode: SearchMode, clicked: Option<usize>, definition: Option<(usize, bool)>) {
        if self.requests.send(Query { generation: self.generation, text, config: self.config.clone(),
            mode, clicked, definition }).is_err() { self.set_status("Search stopped. Reopen the application to retry."); }
    }

    fn poll_definitions(&mut self) {
        for index in 0..self.definitions.len() {
            match self.definitions[index].poll() {
                Ok(Some(Action::Back)) => {
                    self.generation = self.generation.wrapping_add(1);
                    self.definitions.truncate(index); self.hover = None; self.restore_focus(); return;
                }
                Ok(Some(Action::Lookup(query))) => {
                    if index >= 15 { self.set_status("Use Back before opening another nested definition."); return; }
                    self.generation = self.generation.wrapping_add(1);
                    self.hover = None;
                    self.enqueue(query, SearchMode::Dictionary, None, Some((index, false))); return;
                }
                Err(error) => self.set_status(&format!("Definition display failed: {error:#}")),
                _ => {}
            }
        }
        let hovered = self.definitions.iter().rposition(SearchPopup::contains_pointer);
        let next = hovered.and_then(|index| self.definitions[index].hover().map(|query| (index, query)));
        let Some((index, query)) = next else {
            if self.hover.as_ref().is_some_and(|(_, _, _, sent)| *sent) {
                self.generation = self.generation.wrapping_add(1);
            }
            self.hover = None; return;
        };
        if self.hover.as_ref().is_none_or(|(old, text, _, _)| *old != index || *text != query) {
            self.generation = self.generation.wrapping_add(1);
            self.hover = Some((index, query, Instant::now(), false)); return;
        }
        if let Some((index, query, since, sent)) = &mut self.hover {
            if !*sent && since.elapsed() >= Duration::from_millis(350) && self.definitions.len() < 16 {
                *sent = true;
                let (index, query) = (*index, query.clone());
                self.generation = self.generation.wrapping_add(1);
                self.enqueue(query, SearchMode::Dictionary, None, Some((index, true)));
            }
        }
    }

    fn highlight(&self, range: Range<usize>) {
        let text = read_text(self.state.sentence.get());
        let Some(range) = utf16_range(&text, range) else { return; };
        // SAFETY: EM_SETSEL receives bounded UTF-16 offsets for this edit.
        unsafe { SendMessageW(self.state.sentence.get(), EM_SETSEL,
            Some(WPARAM(range.start)), Some(LPARAM(range.end as isize))); }
    }

    fn clear_results(&mut self) {
        self.result = SearchResult::Empty;
        // SAFETY: This thread owns the listbox.
        unsafe { SendMessageW(self.state.results.get(), LB_RESETCONTENT, None, None); }
    }

    fn render_candidates(&self) {
        let rows = candidates(&self.result);
        // SAFETY: The listbox copies each terminated string synchronously.
        unsafe {
            SendMessageW(self.state.results.get(), LB_RESETCONTENT, None, None);
            for candidate in &rows {
                let text = format!("{}\t{}\n{}", candidate.headword, candidate.reading, candidate.summary);
                SendMessageW(self.state.results.get(), LB_ADDSTRING, None, Some(LPARAM(wide(&text).as_ptr() as isize)));
            }
        }
        let status = match &self.result {
            SearchResult::Empty if self.state.mode.get() == SearchMode::Sentence => "Paste a sentence, then click a word to see its candidates.".into(),
            SearchResult::Empty => "Type a Japanese word or expression.".into(),
            SearchResult::Miss => "No matching entries in the enabled dictionaries.".into(),
            SearchResult::Found(_) => format!("{} {}. Select a word to view its definition.",
                rows.len(), if rows.len() == 1 { "candidate" } else { "candidates" }),
        };
        self.set_status(&status);
    }

    fn set_status(&self, text: &str) {
        // SAFETY: SetWindowTextW copies the buffer before returning.
        unsafe { let _ = SetWindowTextW(self.state.status.get(), PCWSTR(wide(text).as_ptr())); }
    }
}

impl Drop for SearchWindow {
    fn drop(&mut self) {
        self.definitions.clear();
        // SAFETY: State and GDI resources outlive native destruction.
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}

fn search(database: &Path, rules: &Path, query: &Query, analyzer: &mut SentenceAnalyzer) -> Reply {
    let tokens = if query.mode == SearchMode::Sentence { analyzer.tokenize(&query.text) } else { Vec::new() };
    let selected = query.clicked.and_then(|offset| tokens.iter()
        .find(|token| token.selectable && token.range.contains(&offset)).map(|token| token.range.clone()));
    let text = if query.mode == SearchMode::Dictionary { query.text.as_str() }
        else { selected.as_ref().and_then(|range| query.text.get(range.clone())).unwrap_or("") };
    let result = SearchService::open(database, rules, &query.config)
        .and_then(|service| service.search(text)).map_err(|error| format!("{error:#}"));
    Reply { generation: query.generation, result, tokens, selected, definition: query.definition }
}

fn utf16_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    if range.start > range.end { return None; }
    Some(text.get(..range.start)?.encode_utf16().count()..text.get(..range.end)?.encode_utf16().count())
}

fn byte_offset(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units + ch.len_utf16() > offset { return byte; }
        units += ch.len_utf16();
    }
    text.len()
}

fn read_text(hwnd: HWND) -> String {
    // SAFETY: GetWindowTextW respects this live buffer's capacity.
    unsafe {
        let mut text = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let length = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        String::from_utf16_lossy(&text[..length])
    }
}

fn scaled(value: f32, scale: f32) -> i32 {
    (value * scale).round().max(1.0) as i32
}

fn card_height(palette: &Palette) -> i32 {
    let pad = scaled(palette.theme.padding as f32, palette.scale).max(4);
    let head = scaled(palette.theme.headword_size.max(palette.theme.reading_size), palette.scale);
    let summary = scaled(palette.theme.collapsed_size, palette.scale);
    head + summary + pad * 2 + scaled(8.0, palette.scale)
}

unsafe fn apply_fonts(state: &State) {
    // SAFETY: The owner thread applies live Palette fonts to live controls.
    unsafe {
        let palette = state.palette.borrow();
        let input_font = if state.mode.get() == SearchMode::Sentence {
            palette.sentence_font
        } else {
            palette.body_font
        };
        for (control, font) in [
            (state.input.get(), input_font),
            (state.sentence.get(), palette.sentence_font),
            (state.results.get(), palette.body_font),
            (state.button.get(), palette.body_font),
            (state.mode_button.get(), palette.body_font),
            (state.status.get(), palette.dimmed_font),
            (state.input_label.get(), palette.dimmed_font),
            (state.sentence_label.get(), palette.dimmed_font),
            (state.results_label.get(), palette.dimmed_font),
        ] {
            SendMessageW(control, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
    }
}

unsafe fn draw_control_frame(parent: HWND, control: HWND, dc: HDC, palette: &Palette) {
    // SAFETY: Coordinates and drawing target belong to the live parent window.
    unsafe {
        if !IsWindowVisible(control).as_bool() { return; }
        let mut bounds = RECT::default();
        if GetWindowRect(control, &mut bounds).is_err() { return; }
        let mut top_left = POINT { x: bounds.left, y: bounds.top };
        let mut bottom_right = POINT { x: bounds.right, y: bounds.bottom };
        if !ScreenToClient(parent, &mut top_left).as_bool()
            || !ScreenToClient(parent, &mut bottom_right).as_bool() {
            return;
        }
        let width = scaled(palette.theme.border_width, palette.scale).max(1);
        let bounds = RECT { left: top_left.x - width, top: top_left.y - width,
            right: bottom_right.x + width, bottom: bottom_right.y + width };
        let top = RECT { bottom: bounds.top + width, ..bounds };
        let bottom = RECT { top: bounds.bottom - width, ..bounds };
        let left = RECT { right: bounds.left + width, ..bounds };
        let right = RECT { left: bounds.right - width, ..bounds };
        for edge in [top, bottom, left, right] { FillRect(dc, &edge, palette.border); }
    }
}

unsafe fn draw_button(item: &DRAWITEMSTRUCT, palette: &Palette) {
    // SAFETY: WM_DRAWITEM supplies a valid DC and rectangle for this button.
    unsafe {
        FillRect(item.hDC, &item.rcItem, palette.background);
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let fill = if pressed { palette.accent } else { palette.background };
        let old_brush = SelectObject(item.hDC, fill.into());
        let old_pen = SelectObject(item.hDC, palette.border_pen.into());
        let radius = scaled(palette.theme.corner_radius as f32, palette.scale).max(2);
        let _ = RoundRect(item.hDC, item.rcItem.left, item.rcItem.top,
            item.rcItem.right, item.rcItem.bottom, radius, radius);
        let _ = SelectObject(item.hDC, old_pen);
        let _ = SelectObject(item.hDC, old_brush);
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(item.hDC, color(if pressed {
            palette.theme.background
        } else {
            palette.theme.body_text
        }));
        let old_font = SelectObject(item.hDC, palette.body_font.into());
        let mut text = wide(&read_text(item.hwndItem));
        text.pop();
        let mut text_rect = item.rcItem;
        DrawTextW(item.hDC, &mut text, &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        let _ = SelectObject(item.hDC, old_font);
        if item.itemState.0 & ODS_FOCUS.0 != 0 {
            let mut focus = item.rcItem;
            let inset = scaled(3.0, palette.scale);
            let _ = InflateRect(&mut focus, -inset, -inset);
            let _ = DrawFocusRect(item.hDC, &focus);
        }
    }
}

unsafe fn draw_candidate(item: &DRAWITEMSTRUCT, palette: &Palette) {
    // SAFETY: WM_DRAWITEM supplies a valid DC and listbox item index.
    unsafe {
        let length = SendMessageW(item.hwndItem, LB_GETTEXTLEN,
            Some(WPARAM(item.itemID as usize)), None).0;
        let mut buffer = vec![0; length.max(0) as usize + 1];
        SendMessageW(item.hwndItem, LB_GETTEXT, Some(WPARAM(item.itemID as usize)),
            Some(LPARAM(buffer.as_mut_ptr() as isize)));
        let text = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
        let (top, summary) = text.split_once('\n').unwrap_or((&text, ""));
        let (headword, reading) = top.split_once('\t').unwrap_or((top, ""));
        let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
        FillRect(item.hDC, &item.rcItem, palette.background);
        let gap = scaled(3.0, palette.scale);
        let mut card = item.rcItem;
        card.left += gap;
        card.right -= gap;
        card.top += gap;
        card.bottom -= gap;
        let fill = if selected { palette.accent } else { palette.background };
        let old_brush = SelectObject(item.hDC, fill.into());
        let old_pen = SelectObject(item.hDC, palette.border_pen.into());
        let radius = scaled(palette.theme.corner_radius as f32, palette.scale).max(2);
        let _ = RoundRect(item.hDC, card.left, card.top, card.right, card.bottom,
            radius, radius);
        let _ = SelectObject(item.hDC, old_pen);
        let _ = SelectObject(item.hDC, old_brush);
        SetBkMode(item.hDC, TRANSPARENT);
        let pad = scaled(palette.theme.padding as f32, palette.scale).max(4);
        let head_height = scaled(
            palette.theme.headword_size.max(palette.theme.reading_size) + 4.0,
            palette.scale,
        );
        let mut head_rect = RECT { left: card.left + pad, top: card.top + pad,
            right: card.right - pad, bottom: card.top + pad + head_height };
        let role_color = |value| color(if selected { palette.theme.background } else { value });
        SetTextColor(item.hDC, role_color(palette.theme.headword_text));
        let old_font = SelectObject(item.hDC, palette.headword_font.into());
        let mut headword = wide(headword);
        headword.pop();
        let mut measured = RECT::default();
        DrawTextW(item.hDC, &mut headword.clone(), &mut measured,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        DrawTextW(item.hDC, &mut headword, &mut head_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        let _ = SelectObject(item.hDC, palette.reading_font.into());
        SetTextColor(item.hDC, role_color(palette.theme.reading_text));
        let mut reading_rect = head_rect;
        reading_rect.left = (reading_rect.left + measured.right + pad).min(reading_rect.right);
        let mut reading = wide(reading);
        reading.pop();
        DrawTextW(item.hDC, &mut reading, &mut reading_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        let _ = SelectObject(item.hDC, palette.summary_font.into());
        SetTextColor(item.hDC, role_color(palette.theme.collapsed_text));
        let mut summary_rect = RECT { left: card.left + pad, top: head_rect.bottom,
            right: card.right - pad, bottom: card.bottom - pad };
        let mut summary = wide(summary);
        summary.pop();
        DrawTextW(item.hDC, &mut summary, &mut summary_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        let _ = SelectObject(item.hDC, old_font);
        if item.itemState.0 & ODS_FOCUS.0 != 0 { let _ = DrawFocusRect(item.hDC, &card); }
    }
}

unsafe fn layout(hwnd: HWND, state: &State) {
    // SAFETY: The caller owns hwnd, State, and every child HWND.
    unsafe {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let palette = state.palette.borrow();
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0),
            (palette.theme.opacity.clamp(0.0, 1.0) * 255.0).round() as u8, LWA_ALPHA);
        let pad = ((palette.theme.padding as f32 * palette.scale).round() as i32).max(4);
        let line = ((palette.theme.body_size * palette.scale).ceil() as i32 + pad * 2).max(32);
        let label_h = (scaled(palette.theme.dimmed_size + 6.0, palette.scale)).max(20);
        let width = (rect.right - pad * 2).max(80);
        let sentence = state.mode.get() == SearchMode::Sentence;
        let input_h = if sentence { (line * 4).max(scaled(160.0, palette.scale)) }
            else { (line * 2).max(scaled(72.0, palette.scale)) };
        let top = pad.max(scaled(12.0, palette.scale));
        let gap = scaled(8.0 + palette.theme.border_width, palette.scale);
        let _ = MoveWindow(state.input_label.get(), pad, top, width, label_h, true);
        let input_y = top + label_h + gap;
        let _ = MoveWindow(state.input.get(), pad, input_y, width, input_h, true);
        let buttons_y = input_y + input_h + pad;
        let button_w = (150.0 * palette.scale).round() as i32;
        let _ = MoveWindow(state.button.get(), pad, buttons_y, button_w, line, true);
        let _ = MoveWindow(state.mode_button.get(), pad * 2 + button_w, buttons_y,
            (190.0 * palette.scale).round() as i32, line, true);
        let sentence_label_y = buttons_y + line + pad;
        let _ = ShowWindow(state.sentence_label.get(), if sentence { SW_SHOW } else { SW_HIDE });
        let _ = MoveWindow(state.sentence_label.get(), pad, sentence_label_y, width, label_h, true);
        let sentence_y = sentence_label_y + label_h + gap;
        let _ = ShowWindow(state.sentence.get(), if sentence { SW_SHOW } else { SW_HIDE });
        let _ = MoveWindow(state.sentence.get(), pad, sentence_y, width, line * 3, true);
        let status_y = if sentence { sentence_y + line * 3 + pad } else { sentence_y };
        let _ = MoveWindow(state.status.get(), pad, status_y, width, line, true);
        let results_label_y = status_y + line;
        let _ = MoveWindow(state.results_label.get(), pad, results_label_y, width, label_h, true);
        let results_y = results_label_y + label_h;
        let _ = MoveWindow(state.results.get(), pad, results_y, width, (rect.bottom - results_y - pad).max(line), true);
        SendMessageW(state.results.get(), LB_SETITEMHEIGHT, Some(WPARAM(0)),
            Some(LPARAM(card_height(&palette) as isize)));
        for edit in [state.input.get(), state.sentence.get()] {
            let _ = GetClientRect(edit, &mut rect);
            rect.left += pad; rect.right -= pad; rect.top += pad; rect.bottom -= pad;
            SendMessageW(edit, EM_SETRECT, None, Some(LPARAM(&rect as *const RECT as isize)));
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: Win32 owns creation arguments. State remains stable until child destruction.
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
                let input = create(w!("EDIT"), w!(""), WS_TABSTOP | WS_VSCROLL
                    | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL) as u32), INPUT);
                let button = create(w!("BUTTON"), w!("Search"), WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), SUBMIT);
                let results = create(w!("LISTBOX"), w!(""), WS_VSCROLL | WS_TABSTOP
                    | WINDOW_STYLE((LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT) as u32), RESULTS);
                let sentence = create(w!("EDIT"), w!(""), WS_VSCROLL | WS_TABSTOP
                    | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_NOHIDESEL) as u32), SENTENCE);
                let status = create(w!("STATIC"), w!(""), WINDOW_STYLE(0), STATUS);
                let mode = create(w!("BUTTON"), w!("Sentence search"), WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32), MODE);
                let input_label = create(w!("STATIC"), w!("Search term"), WINDOW_STYLE(0), INPUT_LABEL);
                let sentence_label = create(w!("STATIC"), w!("Select a word"), WINDOW_STYLE(0), SENTENCE_LABEL);
                let results_label = create(w!("STATIC"), w!("Dictionary candidates"), WINDOW_STYLE(0), RESULTS_LABEL);
                let (Ok(input), Ok(button), Ok(results), Ok(sentence), Ok(status), Ok(mode),
                    Ok(input_label), Ok(sentence_label), Ok(results_label)) =
                    (input, button, results, sentence, status, mode, input_label, sentence_label,
                        results_label) else { return LRESULT(-1) };
                state.input.set(input); state.button.set(button); state.results.set(results);
                state.sentence.set(sentence); state.status.set(status); state.mode_button.set(mode);
                state.input_label.set(input_label); state.sentence_label.set(sentence_label);
                state.results_label.set(results_label);
                apply_fonts(state);
                for control in [input, button, results, sentence, status, mode, input_label,
                    sentence_label, results_label] {
                    let _ = SetWindowTheme(control, w!(""), w!(""));
                }
                for edit in [input, sentence] {
                    SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(16000)), None);
                    if !SetWindowSubclass(edit, Some(input_proc), 1, pointer as usize).as_bool() { return LRESULT(-1); }
                }
                if !SetWindowSubclass(results, Some(input_proc), 1, pointer as usize).as_bool() { return LRESULT(-1); }
                LRESULT(0)
            }
            WM_SIZE => { layout(hwnd, state); LRESULT(0) }
            WM_TIMER => LRESULT(0),
            WM_DPICHANGED => {
                let rect = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, rect.left, rect.top,
                    rect.right - rect.left, rect.bottom - rect.top, SWP_NOZORDER | SWP_NOACTIVATE);
                state.dpi_changed.set(true); LRESULT(0)
            }
            WM_COMMAND => {
                let id = wp.0 & 0xffff;
                let notification = (wp.0 >> 16) as u32;
                if id == INPUT && notification == EN_CHANGE {
                    state.preserve_definitions.set(false);
                    state.clicked.set(None);
                    state.submit.set(true);
                }
                if id == SUBMIT && !state.composing.get() {
                    state.preserve_definitions.set(false);
                    state.submit.set(true);
                }
                if id == MODE {
                    state.preserve_definitions.set(false);
                    state.switch.set(true);
                }
                if id == RESULTS && notification == LBN_SELCHANGE {
                    state.selected.set(usize::try_from(SendMessageW(state.results.get(), LB_GETCURSEL, None, None).0).ok());
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                let mut rect = RECT::default(); let _ = GetClientRect(hwnd, &mut rect);
                FillRect(HDC(wp.0 as *mut std::ffi::c_void), &rect, state.palette.borrow().background);
                LRESULT(1)
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let palette = state.palette.borrow();
                draw_control_frame(hwnd, state.input.get(), dc, &palette);
                draw_control_frame(hwnd, state.sentence.get(), dc, &palette);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                let palette = state.palette.borrow();
                let dc = HDC(wp.0 as *mut std::ffi::c_void);
                SetTextColor(dc, color(palette.theme.body_text)); SetBkColor(dc, color(palette.theme.background));
                LRESULT(palette.background.0 as isize)
            }
            WM_CTLCOLORSTATIC => {
                let palette = state.palette.borrow();
                let dc = HDC(wp.0 as *mut std::ffi::c_void);
                SetTextColor(dc, color(palette.theme.dimmed_text));
                SetBkColor(dc, color(palette.theme.background));
                LRESULT(palette.background.0 as isize)
            }
            WM_MEASUREITEM => {
                let item = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
                if item.CtlID == RESULTS as u32 {
                    item.itemHeight = card_height(&state.palette.borrow()) as u32;
                }
                LRESULT(1)
            }
            WM_DRAWITEM => {
                let item = &*(lp.0 as *const DRAWITEMSTRUCT);
                let palette = state.palette.borrow();
                if item.CtlID == RESULTS as u32 {
                    if item.itemID != u32::MAX { draw_candidate(item, &palette); }
                } else if item.CtlID == SUBMIT as u32 || item.CtlID == MODE as u32 {
                    draw_button(item, &palette);
                }
                LRESULT(1)
            }
            WM_ACTIVATE if wp.0 & 0xffff != WA_INACTIVE as usize => {
                crate::input::hooks::clear_keyboard_actions(); DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SETFOCUS => { let _ = SetFocus(Some(state.input.get())); LRESULT(0) }
            WM_CLOSE => {
                state.dismissed.set(true);
                state.submit.set(false);
                state.switch.set(false);
                state.clicked.set(None);
                state.selected.set(None);
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_NCDESTROY => { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0); DefWindowProcW(hwnd, msg, wp, lp) }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn input_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM,
    _id: usize, data: usize) -> LRESULT {
    // SAFETY: The parent owns State until both edits and the results listbox are destroyed.
    unsafe {
        let state = &*(data as *const State);
        if msg == WM_KEYDOWN && wp.0 == 27 && !state.composing.get() {
            if lp.0 & (1 << 30) != 0 { return LRESULT(0); }
            state.dismissed.set(true);
            state.submit.set(false);
            state.switch.set(false);
            state.clicked.set(None);
            state.selected.set(None);
            if let Ok(owner) = GetParent(hwnd) {
                let _ = PostMessageW(Some(owner), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        if hwnd == state.results.get() && msg == WM_LBUTTONUP {
            let result = DefSubclassProc(hwnd, msg, wp, lp);
            let row = SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lp)).0 as u32;
            if row >> 16 == 0 { state.selected.set(Some((row & 0xffff) as usize)); }
            return result;
        }
        if hwnd == state.sentence.get() && msg == WM_LBUTTONUP {
            let result = DefSubclassProc(hwnd, msg, wp, lp);
            let offset = SendMessageW(hwnd, EM_CHARFROMPOS, None, Some(lp)).0 as u32 & 0xffff;
            state.clicked.set(Some(byte_offset(&read_text(hwnd), offset as usize)));
            return result;
        }
        if hwnd == state.input.get() {
            match msg {
                WM_SETFOCUS => {
                    let result = DefSubclassProc(hwnd, msg, wp, lp);
                    let mut point = POINT::default();
                    if GetCaretPos(&mut point).is_ok() {
                        let palette = state.palette.borrow();
                        let size = if state.mode.get() == SearchMode::Sentence {
                            (palette.theme.body_size * 1.35).max(palette.theme.headword_size + 2.0)
                                .max(palette.theme.collapsed_size + 2.0)
                        } else { palette.theme.body_size };
                        if CreateCaret(hwnd, None, scaled(2.0, palette.scale), scaled(size, palette.scale)).is_ok() {
                            let _ = SetCaretPos(point.x, point.y);
                            let _ = ShowCaret(Some(hwnd));
                        }
                    }
                    return result;
                }
                WM_SETCURSOR if lp.0 as u16 == HTCLIENT as u16 => {
                    if let Ok(cursor) = LoadCursorW(None, IDC_IBEAM) { let _ = SetCursor(Some(cursor)); }
                    return LRESULT(1);
                }
                WM_IME_START => state.composing.set(true),
                WM_IME_END => { state.composing.set(false); state.submit.set(true); }
                WM_KEYDOWN if wp.0 == 13 && !state.composing.get() && state.mode.get() == SearchMode::Dictionary => {
                    state.submit.set(true); return LRESULT(0);
                }
                WM_CHAR if wp.0 == 13 && !state.composing.get() && state.mode.get() == SearchMode::Dictionary => return LRESULT(0),
                _ => {}
            }
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_sentence_offsets_cover_surrogates_and_reject_split_bytes() {
        let text = "A𠮷猫\r\n犬";
        assert_eq!(utf16_range(text, 1..8), Some(1..4));
        assert_eq!(utf16_range(text, 2..8), None);
        assert_eq!(byte_offset(text, 2), 1);
        assert_eq!(byte_offset(text, 3), 5);
        assert_eq!(byte_offset(text, 6), 10);
    }

    struct Fixture(PathBuf);
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }

    fn fixture() -> (SearchWindow, Fixture, impl Sized) {
        let guard = crate::input::hooks::search_keyboard_test_guard();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let database = std::env::temp_dir().join(format!("chibipop-native-search-{}.sqlite", std::process::id()));
        chibipop::dict::build::build(&[root.join("tests/fixtures/yomitan/terms.zip")], &[], &database, &|_| {}).unwrap();
        let window = SearchWindow::open(&database, &root.join("data/deconjugator.json"), &Config::default()).unwrap();
        (window, Fixture(database), guard)
    }

    fn wait(window: &mut SearchWindow, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let mut message = MSG::default();
            // SAFETY: Only this test thread's queued messages are dispatched.
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    if !window.handle_message(&message) { let _ = TranslateMessage(&message); DispatchMessageW(&message); }
                }
            }
            window.poll();
            let ready = if expected == "candidate" { matches!(window.result, SearchResult::Found(_)) }
                else { read_text(window.state.status.get()).contains(expected) };
            if ready { return; }
            assert!(Instant::now() < deadline, "expected {expected:?}, got {:?}", read_text(window.state.status.get()));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn replace_input(window: &SearchWindow, value: &str) {
        let value = wide(value);
        // SAFETY: The edit owns its text. Replacement sends the normal edit-change notification.
        unsafe {
            SendMessageW(window.state.input.get(), EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            SendMessageW(window.state.input.get(), EM_REPLACESEL, Some(WPARAM(0)), Some(LPARAM(value.as_ptr() as isize)));
        }
    }

    #[test]
    #[ignore = "Moves the real desktop pointer over a native search definition"]
    fn native_definition_hover_and_disable_toggle() {
        let (mut window, fixture, _guard) = fixture();
        let connection = rusqlite::Connection::open(&fixture.0).unwrap();
        connection.execute("UPDATE entry SET glossary = ?1 WHERE entry_id IN (SELECT entry_id FROM term WHERE written = '猫')",
            [r#"[{"type":"structured-content","content":[{"tag":"ruby","content":["食",{"tag":"rt","content":"た"}]},"べる"]}]"#]).unwrap();
        drop(connection);
        // SAFETY: This test owns the edit control and its text buffer.
        unsafe { SetWindowTextW(window.state.input.get(), w!("猫")).unwrap(); }
        wait(&mut window, "candidate");
        let candidate = candidates(&window.result).iter().find(|candidate| candidate.headword == "猫").unwrap().index;
        window.state.selected.set(Some(candidate)); window.poll();
        assert_eq!(window.definitions.len(), 1);
        struct RestorePointer(POINT);
        impl Drop for RestorePointer {
            fn drop(&mut self) {
                // SAFETY: Restore only the position saved by this explicit desktop test.
                unsafe { let _ = SetCursorPos(self.0.x, self.0.y); }
            }
        }
        let mut saved = POINT::default();
        // SAFETY: This live buffer receives the current desktop pointer.
        unsafe { GetCursorPos(&mut saved).unwrap(); }
        let _restore = RestorePointer(saved);
        let origin = window.definitions[0].anchor().unwrap();
        let mut chosen = None;
        for offset in (6..210).step_by(6) {
            // SAFETY: This explicit desktop test targets its own definition window.
            unsafe { SetCursorPos(origin.x + 20, origin.y + offset).unwrap(); }
            let deadline = Instant::now() + Duration::from_millis(450);
            while Instant::now() < deadline {
                let mut message = MSG::default();
                // SAFETY: Dispatch messages only for the current test thread.
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message); DispatchMessageW(&message);
                    }
                }
                window.poll(); std::thread::sleep(Duration::from_millis(10));
            }
            if window.definitions.len() == 2 { chosen = Some(offset); break; }
        }
        let offset = chosen.expect("hover over ruby-backed 食べる must open a child definition");
        let mut config = Config::default(); config.popup.sub_popups = false;
        window.update_config(&config); wait(&mut window, "candidate");
        window.state.selected.set(Some(candidate)); window.poll();
        let origin = window.definitions[0].anchor().unwrap();
        // SAFETY: This explicit desktop test targets its own definition window.
        unsafe { SetCursorPos(origin.x + 20, origin.y + offset).unwrap(); }
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline { wait(&mut window, "candidate"); std::thread::sleep(Duration::from_millis(20)); }
        assert_eq!(window.definitions.len(), 1);
    }

    #[test]
    fn native_live_candidates_definition_sentence_and_reopen() {
        let (mut window, _fixture, _guard) = fixture();
        // SAFETY: Test controls and strings remain live during these calls.
        unsafe { SetWindowTextW(window.state.input.get(), w!("食べました")).unwrap(); }
        wait(&mut window, "candidate");
        assert!(candidates(&window.result).iter().any(|row| row.headword == "食べる"));
        // SAFETY: This selects a real list row and emits its native notification.
        unsafe {
            SendMessageW(window.state.results.get(), LB_SETCURSEL, Some(WPARAM(0)), None);
            SendMessageW(window.hwnd, WM_COMMAND,
                Some(WPARAM(RESULTS | ((LBN_SELCHANGE as usize) << 16))),
                Some(LPARAM(window.state.results.get().0 as isize)));
        }
        window.poll(); assert_eq!(window.definitions.len(), 1);
        window.switch_mode(SearchMode::Sentence, Some("猫 犬"));
        wait(&mut window, "click a word");
        let mut rect = RECT::default();
        // SAFETY: EM_POSFROMCHAR resolves real pixels for the first character.
        unsafe {
            SendMessageW(window.state.sentence.get(), EM_GETRECT, None, Some(LPARAM(&mut rect as *mut RECT as isize)));
            let point = SendMessageW(window.state.sentence.get(), EM_POSFROMCHAR, Some(WPARAM(0)), None);
            SendMessageW(window.state.sentence.get(), WM_LBUTTONUP, None, Some(LPARAM(point.0 + 1)));
        }
        window.poll(); wait(&mut window, "candidate");
        assert!(candidates(&window.result).iter().any(|row| row.headword == "猫"));
        let mut start = 0u32; let mut end = 0u32;
        // SAFETY: EM_GETSEL writes two valid scalar output pointers.
        unsafe {
            SendMessageW(window.state.sentence.get(), EM_GETSEL,
                Some(WPARAM(&mut start as *mut u32 as usize)), Some(LPARAM(&mut end as *mut u32 as isize)));
            SendMessageW(window.hwnd, WM_CLOSE, None, None);
        }
        assert_eq!((start, end), (0, 1)); assert!(!window.is_visible());
        window.show(); assert!(window.is_visible());
        drop(window);
    }

    #[test]
    fn native_ime_stale_results_empty_miss_and_config() {
        let (mut window, _fixture, _guard) = fixture();
        window.state.submit.set(false);
        // SAFETY: Messages target this test thread's native edit.
        unsafe {
            SendMessageW(window.state.input.get(), WM_IME_START, None, None);
            SendMessageW(window.state.input.get(), WM_KEYDOWN, Some(WPARAM(13)), None);
        }
        assert!(!window.state.submit.get());
        let escape = MSG { hwnd: window.state.input.get(), message: WM_KEYDOWN, wParam: WPARAM(27), ..Default::default() };
        assert!(!window.handle_message(&escape));
        // SAFETY: Setters copy text on the owner thread.
        unsafe { SendMessageW(window.state.input.get(), WM_IME_END, None, None); SetWindowTextW(window.state.input.get(), w!("猫")).unwrap(); }
        wait(&mut window, "candidate");
        let mut config = Config::default();
        config.dictionaries.terms_disabled = vec!["FixtureTerms".into()];
        window.update_config(&config); wait(&mut window, "No matching entries");
        window.update_config(&Config::default()); wait(&mut window, "candidate");
        // SAFETY: These are synchronous native text changes.
        replace_input(&window, "　 ");
        wait(&mut window, "Type a Japanese word");
        // SAFETY: These are synchronous native text changes.
        replace_input(&window, "絶対にない検索");
        wait(&mut window, "No matching entries");
        // SAFETY: These are synchronous native text changes.
        replace_input(&window, "猫");
        wait(&mut window, "candidate");
        let (sender, replies) = mpsc::channel(); window.replies = replies;
        sender.send(Reply { generation: window.generation.wrapping_sub(1), result: Ok(SearchResult::Miss),
            tokens: vec![], selected: None, definition: None }).unwrap();
        window.poll(); assert!(matches!(window.result, SearchResult::Found(_)));
        window.state.selected.set(Some(0));
        window.poll();
        assert_eq!(1, window.definitions.len());
        let cancelled_generation = window.generation;
        sender.send(Reply { generation: cancelled_generation, result: Ok(SearchResult::Miss),
            tokens: vec![], selected: None, definition: None }).unwrap();
        // SAFETY: This query reads the current test thread's actual keyboard focus.
        let focused = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        assert_eq!(focused, window.definitions[0].hwnd());
        let definition_escape = MSG { hwnd: focused, message: WM_KEYDOWN,
            wParam: WPARAM(27), ..Default::default() };
        assert!(window.handle_message(&definition_escape));
        assert!(window.is_visible());
        assert_ne!(cancelled_generation, window.generation);
        assert!(window.definitions.is_empty());
        // SAFETY: This query reads the current test thread's restored focus.
        assert_eq!(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() }, window.state.input.get());
        window.poll();
        assert!(matches!(window.result, SearchResult::Found(_)));
        assert!(window.handle_message(&escape));
        assert!(!window.is_visible());
        drop(window);
    }
}
