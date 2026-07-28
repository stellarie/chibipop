//! The settings window: eleven settings, a dictionary order, Apply and Cancel.
//!
//! **Modeless, and that is a safety property rather than a preference.**
//! `DialogBoxParamW` runs its own message pump, and a nested pump on the main
//! thread stops `WM_TIMER` arriving while the low-level hook keeps firing —
//! which latches `input::hooks`'s scroll arm and kills the scroll wheel for
//! every application (popup-interaction spec D9, reproduced there from
//! `TrackPopupMenuEx`). A tray menu holds that for a second; a settings window
//! would hold it for minutes. So this is an ordinary `CreateWindowExW` window
//! serviced by `app::run`'s existing loop through `IsDialogMessageW`.
//!
//! **Nothing here is modal, including the error paths.** `MessageBoxW` pumps
//! too; failures are reported by `app::run` on stderr instead.
//!
//! **Numbers are combo boxes, not spin controls.** A spin control is
//! `msctls_updown32`, which needs `InitCommonControlsEx` and therefore the
//! `Win32_UI_Controls` feature. Offering the permitted values in a dropdown
//! keeps the spec's "no free-text entry anywhere" guarantee with one control
//! type instead of three, needs no common-control initialisation at all, and
//! avoids widening the crate's Windows feature set for three numbers.
//!
//! This module holds no opinion about what any setting means: it is handed a
//! [`SettingsForm`] and hands one back.

use crate::settings::{SettingsForm, MAX_HEIGHT_RANGE, PASSES_RANGE, SUMMARY_RANGE};
use anyhow::{Context, Result};
use std::cell::Cell;
use windows::core::{w, Error, PCWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, EnumFontFamiliesExW, GetDC, ReleaseDC, COLOR_BTNFACE,
    ENUMLOGFONTEXW, HFONT, LOGFONTW, SHIFTJIS_CHARSET, TEXTMETRICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// What the user did with the window. Read and cleared by `app::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    Apply,
    Cancel,
}

// ---- control ids ----

const ID_APPLY: i32 = 100;
const ID_CANCEL: i32 = 101;
const ID_MODE_LIVE: i32 = 102;
const ID_MODE_HOLD: i32 = 103;
const ID_THEME: i32 = 104;
const ID_FONT: i32 = 105;
const ID_MAX_HEIGHT: i32 = 106;
const ID_SUMMARY: i32 = 107;
const ID_HIGHLIGHT: i32 = 108;
const ID_SCROLL: i32 = 109;
const ID_EXCLUDE: i32 = 110;
const ID_DICTS: i32 = 111;
const ID_DICT_UP: i32 = 112;
const ID_DICT_DOWN: i32 = 113;
const ID_PASSES: i32 = 114;
const ID_SHOW_SCAN: i32 = 115;

// ---- layout, in unscaled pixels ----

const WIN_W: i32 = 470;
const WIN_H: i32 = 620;
const PAD: i32 = 14;
const ROW_H: i32 = 24;
const ROW_GAP: i32 = 6;
const LABEL_W: i32 = 150;
const FIELD_X: i32 = PAD + LABEL_W;
const FIELD_W: i32 = WIN_W - FIELD_X - PAD - 16;

fn class_name() -> PCWSTR {
    w!("ChibipopSettingsClass")
}

thread_local! {
    // The pending outcome, and which window produced it.
    //
    // A thread-local rather than `GWLP_USERDATA`, matching `ui::overlay`'s
    // choice and for the same reason: every settings window is created and
    // driven from the main thread only, so there is no concurrent access for
    // a lock to guard, and it avoids stashing and later reclaiming a boxed
    // pointer across create/`Drop`. Keyed by `HWND` so a stale outcome from a
    // previous window can never be read by a new one.
    static OUTCOME: Cell<Option<(isize, SettingsOutcome)>> = const { Cell::new(None) };
}

fn record_outcome(hwnd: HWND, outcome: SettingsOutcome) {
    OUTCOME.with(|c| c.set(Some((hwnd.0 as isize, outcome))));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_APPLY => record_outcome(hwnd, SettingsOutcome::Apply),
                // The X button and Escape both land here through IDCANCEL,
                // so closing can never apply by accident.
                ID_CANCEL | 2 => record_outcome(hwnd, SettingsOutcome::Cancel),
                ID_DICT_UP => unsafe { move_selected(hwnd, -1) },
                ID_DICT_DOWN => unsafe { move_selected(hwnd, 1) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            record_outcome(hwnd, SettingsOutcome::Cancel);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Registers the window class exactly once per process - `ui::window`'s
/// ordering rationale (latch `true` only *after* `RegisterClassExW` actually
/// succeeds) applies identically here.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` is a fully-initialised `WNDCLASSEXW` (the `..Default`
    // spread zeroes every field not set here); `lpfnWndProc` points at a
    // `'static extern "system" fn` valid for the process lifetime, which is
    // what the OS requires.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW(IDC_ARROW)")?,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void,
            ),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// The shell's own UI font, so the window looks like every other dialog on the
/// machine rather than like 1995's system font.
///
/// `None` on failure; the caller then leaves controls with the default font,
/// which is ugly but entirely functional.
unsafe fn ui_font() -> Option<HFONT> {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `ncm` is stack storage of exactly the size declared in its own
    // `cbSize` field, which is the contract this call checks.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            Some(&mut ncm as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    if !ok {
        return None;
    }
    // SAFETY: `lfMessageFont` was populated by the call above.
    unsafe { CreateFontIndirectW(&ncm.lfMessageFont) }.into()
}

/// Every installed font family that can render Japanese.
///
/// Enumerated with `SHIFTJIS_CHARSET` so the list cannot offer a face that
/// would draw the popup's text as boxes. Families, not faces — weight and
/// style variants collapse to one entry, which is exactly what
/// `Theme::font_name` can express.
///
/// Names beginning `@` are skipped: those are Windows' vertical-writing
/// duplicates, listed alongside each family and never wanted here.
pub fn japanese_font_families() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // SAFETY: `lf` and `out` are stack/owned data that outlive the call;
    // `EnumFontFamiliesExW` invokes the callback synchronously on this thread
    // for the duration of this call only, so the `&mut Vec` it is handed
    // cannot dangle. The device context is released on every path.
    unsafe {
        let hdc = GetDC(None);
        let lf = LOGFONTW { lfCharSet: SHIFTJIS_CHARSET, ..Default::default() };
        EnumFontFamiliesExW(
            hdc,
            &lf,
            Some(enum_font_cb),
            LPARAM(&mut out as *mut Vec<String> as isize),
            0,
        );
        ReleaseDC(None, hdc);
    }
    out.sort();
    out.dedup();
    out
}

unsafe extern "system" fn enum_font_cb(
    lf: *const LOGFONTW,
    _tm: *const TEXTMETRICW,
    _kind: u32,
    lparam: LPARAM,
) -> i32 {
    // SAFETY: the OS passes a valid `ENUMLOGFONTEXW` and the `lparam` this
    // callback was registered with, which `japanese_font_families` set to a
    // live `&mut Vec<String>` that outlives the enumeration.
    unsafe {
        let elf = &*(lf as *const ENUMLOGFONTEXW);
        let name = String::from_utf16_lossy(
            &elf.elfLogFont.lfFaceName
                [..elf.elfLogFont.lfFaceName.iter().position(|&c| c == 0).unwrap_or(0)],
        );
        if !name.is_empty() && !name.starts_with('@') {
            (*(lparam.0 as *mut Vec<String>)).push(name);
        }
    }
    1
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create one child control and give it the UI font.
#[allow(clippy::too_many_arguments)]
unsafe fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: i32,
    font: Option<HFONT>,
) -> WinResult<HWND> {
    // SAFETY: `parent` is this window's live handle; `text` is copied by the
    // OS during the call; `id` is passed as the menu handle, the documented
    // meaning for a child window.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(wide(text).as_ptr()),
            style | WS_CHILD | WS_VISIBLE,
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as *mut core::ffi::c_void)),
            None,
            None,
        )?
    };
    if let Some(f) = font {
        // SAFETY: `hwnd` was just created; `WM_SETFONT` copies nothing and
        // the font outlives the window (it is destroyed in `Drop`).
        unsafe {
            SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        }
    }
    Ok(hwnd)
}

/// Swap the listbox's selected dictionary with its neighbour.
///
/// The selection **follows the item**, so Move up can be pressed repeatedly
/// without re-selecting; a listbox that kept the selection on the *index*
/// would make reordering three dictionaries a nine-click job.
unsafe fn move_selected(hwnd: HWND, delta: i32) {
    // SAFETY: every call below targets `ID_DICTS`, a live child of `hwnd`
    // created in `open`. The text buffer is stack storage sized to the
    // LB_GETTEXT contract's own reported length.
    unsafe {
        let list = GetDlgItem(Some(hwnd), ID_DICTS).unwrap_or_default();
        if list.is_invalid() {
            return;
        }
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
        let cur = SendMessageW(list, LB_GETCURSEL, None, None).0;
        let target = cur + delta as isize;
        if cur < 0 || target < 0 || target >= count {
            return;
        }
        let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(cur as usize)), None).0;
        if len <= 0 {
            return;
        }
        let mut buf = vec![0u16; len as usize + 1];
        SendMessageW(
            list,
            LB_GETTEXT,
            Some(WPARAM(cur as usize)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        );
        SendMessageW(list, LB_DELETESTRING, Some(WPARAM(cur as usize)), None);
        SendMessageW(
            list,
            LB_INSERTSTRING,
            Some(WPARAM(target as usize)),
            Some(LPARAM(buf.as_ptr() as isize)),
        );
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(target as usize)), None);
        update_move_buttons(hwnd);
    }
}

/// Move up is disabled on the first item and Move down on the last, rather
/// than silently doing nothing.
unsafe fn update_move_buttons(hwnd: HWND) {
    // SAFETY: both ids are live children of `hwnd`, created in `open`.
    unsafe {
        let list = GetDlgItem(Some(hwnd), ID_DICTS).unwrap_or_default();
        if list.is_invalid() {
            return;
        }
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
        let cur = SendMessageW(list, LB_GETCURSEL, None, None).0;
        if let Ok(up) = GetDlgItem(Some(hwnd), ID_DICT_UP) {
            let _ = EnableWindow(up, cur > 0);
        }
        if let Ok(down) = GetDlgItem(Some(hwnd), ID_DICT_DOWN) {
            let _ = EnableWindow(down, cur >= 0 && cur < count - 1);
        }
    }
}

/// The permitted values for a numeric combo, with `current` inserted if it is
/// not already one of them.
///
/// Inserting rather than snapping matters: a hand-edited config holding 43
/// must not silently become 45 merely because the user opened Settings and
/// pressed Apply without touching that control.
fn numeric_choices(lo: i64, hi: i64, step: i64, current: i64) -> Vec<i64> {
    let mut v: Vec<i64> = (lo..=hi).step_by(step as usize).collect();
    if !v.contains(&current) && current >= lo && current <= hi {
        v.push(current);
        v.sort_unstable();
    }
    v
}

pub struct SettingsWindow {
    hwnd: HWND,
    font: Option<HFONT>,
    /// The numeric values each combo offers, in the order they were added, so
    /// `read` can map a selection index back to a value.
    heights: Vec<i64>,
    summaries: Vec<i64>,
    passes: Vec<i64>,
    fonts: Vec<String>,
}

impl SettingsWindow {
    /// Create and show the window, populated from `form`.
    ///
    /// `stale` are `display_order` entries matching no installed dictionary
    /// (spec D6a); when non-empty a warning naming them is shown, because that
    /// is what a dictionary rename looks like from in here.
    pub fn open(form: &SettingsForm, stale: &[String]) -> Result<SettingsWindow> {
        // SAFETY: every call below is an ordinary window-creation FFI call
        // with handles this function owns; each `?` leaves nothing to leak
        // because the window is the only resource and it is not yet created.
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();
            register_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name(),
                w!("chibipop settings"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                WIN_W,
                WIN_H,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for the settings window")?;

            let font = ui_font();
            let mut win = SettingsWindow {
                hwnd,
                font,
                heights: Vec::new(),
                summaries: Vec::new(),
                passes: Vec::new(),
                fonts: Vec::new(),
            };
            win.build(form, stale)?;

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            Ok(win)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Bring an already-open window forward instead of creating a second.
    pub fn focus(&self) {
        // SAFETY: `self.hwnd` is live until `Drop`.
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// The pending Apply/Cancel, cleared by reading.
    pub fn take_outcome(&self) -> Option<SettingsOutcome> {
        OUTCOME.with(|c| match c.get() {
            Some((h, o)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(o)
            }
            _ => None,
        })
    }

    unsafe fn build(&mut self, form: &SettingsForm, stale: &[String]) -> Result<()> {
        let f = self.font;
        let h = self.hwnd;
        let mut y = PAD;

        // SAFETY: `h` is the window just created by `open`; every control is
        // a child of it and lives until the window is destroyed.
        unsafe {
            let group = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(h, w!("BUTTON"), text, WINDOW_STYLE(BS_GROUPBOX as u32),
                      PAD - 6, y, WIN_W - 2 * PAD, height, 0, f)
            };
            let label = |text: &str, y: i32| -> WinResult<HWND> {
                child(h, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y + 4, LABEL_W, ROW_H, 0, f)
            };

            // ---- Trigger ----
            group("Trigger", y, ROW_H + 26)?;
            y += 20;
            let live = child(h, w!("BUTTON"), "Live",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32) | WS_GROUP | WS_TABSTOP,
                PAD, y, 120, ROW_H, ID_MODE_LIVE, f)?;
            let hold = child(h, w!("BUTTON"), "Hold Shift",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
                PAD + 130, y, 160, ROW_H, ID_MODE_HOLD, f)?;
            let is_live = matches!(form.mode, crate::config::TriggerMode::Live);
            SendMessageW(live, BM_SETCHECK,
                Some(WPARAM(if is_live { 1 } else { 0 })), None);
            SendMessageW(hold, BM_SETCHECK,
                Some(WPARAM(if is_live { 0 } else { 1 })), None);
            y += ROW_H + 18;

            // ---- Popup ----
            group("Popup", y, 5 * (ROW_H + ROW_GAP) + 3 * ROW_H + 16)?;
            y += 20;

            label("Theme", y)?;
            let theme = child(h, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_THEME, f)?;
            for (i, name) in ["dark", "light"].iter().enumerate() {
                SendMessageW(theme, CB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
                if form.theme == *name {
                    SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(theme, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H + ROW_GAP;

            label("Font", y)?;
            let fonts_hwnd = child(h, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 260, ID_FONT, f)?;
            let mut families = japanese_font_families();
            // Spec D4: an absent configured font is still offered and
            // selected, so opening Settings and applying cannot silently
            // change a setting the user never touched.
            if !families.iter().any(|x| x == &form.font) {
                families.push(form.font.clone());
                families.sort();
            }
            for (i, name) in families.iter().enumerate() {
                SendMessageW(fonts_hwnd, CB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
                if name == &form.font {
                    SendMessageW(fonts_hwnd, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            self.fonts = families;
            y += ROW_H + ROW_GAP;

            self.heights = numeric_choices(
                MAX_HEIGHT_RANGE.0 as i64, MAX_HEIGHT_RANGE.1 as i64, 5,
                form.max_height_percent as i64);
            label("Max height (% of screen)", y)?;
            let mh = child(h, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_MAX_HEIGHT, f)?;
            fill_numeric(mh, &self.heights, form.max_height_percent as i64);
            y += ROW_H + ROW_GAP;

            self.summaries = numeric_choices(
                SUMMARY_RANGE.0 as i64, SUMMARY_RANGE.1 as i64, 10,
                form.summary_chars as i64);
            label("Summary length (characters)", y)?;
            let sm = child(h, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_SUMMARY, f)?;
            fill_numeric(sm, &self.summaries, form.summary_chars as i64);
            y += ROW_H + ROW_GAP + 4;

            let check = |text: &str, id: i32, on: bool, y: i32| -> WinResult<()> {
                let c = child(h, w!("BUTTON"), text,
                    WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                    PAD, y, WIN_W - 2 * PAD - 20, ROW_H, id, f)?;
                SendMessageW(c, BM_SETCHECK, Some(WPARAM(if on { 1 } else { 0 })), None);
                Ok(())
            };
            check("Box the word being defined", ID_HIGHLIGHT, form.highlight_match, y)?;
            y += ROW_H;
            check("Scroll long entries with the wheel", ID_SCROLL, form.scroll_popup, y)?;
            y += ROW_H;
            check("Hide the popup from screen capture", ID_EXCLUDE,
                  form.exclude_from_capture, y)?;
            y += ROW_H + 18;

            // ---- Dictionaries ----
            let dict_h = 5 * ROW_H + 34;
            group("Dictionaries — topmost is shown first", y, dict_h)?;
            y += 20;
            let list = child(h, w!("LISTBOX"), "",
                WINDOW_STYLE(LBS_NOTIFY as u32) | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
                PAD, y, WIN_W - 2 * PAD - 110, 4 * ROW_H, ID_DICTS, f)?;
            for name in &form.dict_names {
                SendMessageW(list, LB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
            }
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            let bx = WIN_W - PAD - 100;
            child(h, w!("BUTTON"), "Move up", WS_TABSTOP,
                  bx, y, 92, ROW_H, ID_DICT_UP, f)?;
            child(h, w!("BUTTON"), "Move down", WS_TABSTOP,
                  bx, y + ROW_H + 4, 92, ROW_H, ID_DICT_DOWN, f)?;
            y += 4 * ROW_H + 2;
            child(h, w!("STATIC"),
                "Order is matched by dictionary name. If you rebuild your \
                 dictionaries, check this list again.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 28, 0, f)?;
            y += dict_h - 4 * ROW_H - 18;

            // Spec D6a: name the entry, because the visible symptom of a
            // stale one is a dictionary silently sorting last.
            if !stale.is_empty() {
                let msg = format!(
                    "\"{}\" no longer matches any dictionary — it may have been renamed or \
                     removed. Dictionaries it used to order are now sorted last.",
                    stale.join("\", \"")
                );
                child(h, w!("STATIC"), &msg, WINDOW_STYLE(0),
                      PAD, y, WIN_W - 2 * PAD - 20, 32, 0, f)?;
            }
            y += 36;

            // ---- Debug ----
            group("Debug", y, 3 * ROW_H + 34)?;
            y += 20;
            self.passes = numeric_choices(
                PASSES_RANGE.0 as i64, PASSES_RANGE.1 as i64, 1,
                form.max_ocr_passes as i64);
            label("OCR passes per hover", y)?;
            let ps = child(h, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 160, ID_PASSES, f)?;
            fill_numeric(ps, &self.passes, form.max_ocr_passes as i64);
            y += ROW_H;
            child(h, w!("STATIC"),
                "1 = no tiling. Higher reads further ahead but can resolve the wrong character.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 28, 0, f)?;
            y += 28;
            let scan = child(h, w!("BUTTON"), "Outline what each hover captured",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD, y, WIN_W - 2 * PAD - 20, ROW_H, ID_SHOW_SCAN, f)?;
            SendMessageW(scan, BM_SETCHECK,
                Some(WPARAM(if form.show_scan_region { 1 } else { 0 })), None);
            y += ROW_H + 18;

            // ---- Apply / Cancel ----
            child(h, w!("STATIC"),
                "Applying saves your settings and restarts chibipop.",
                WINDOW_STYLE(0), PAD, y + 6, 250, ROW_H, 0, f)?;
            child(h, w!("BUTTON"), "Apply & Restart",
                  WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
                  WIN_W - PAD - 230, y, 120, ROW_H + 4, ID_APPLY, f)?;
            child(h, w!("BUTTON"), "Cancel", WS_TABSTOP,
                  WIN_W - PAD - 104, y, 96, ROW_H + 4, ID_CANCEL, f)?;

            update_move_buttons(h);
        }
        Ok(())
    }

    /// The controls' current values, as a form.
    pub fn read(&self, template: &SettingsForm) -> SettingsForm {
        // SAFETY: every id below is a live child of `self.hwnd`, created in
        // `build` and destroyed only with the window in `Drop`.
        unsafe {
            let h = self.hwnd;
            let checked = |id: i32| -> bool {
                GetDlgItem(Some(h), id)
                    .map(|c| SendMessageW(c, BM_GETCHECK, None, None).0 == 1)
                    .unwrap_or(false)
            };
            let combo_index = |id: i32| -> isize {
                GetDlgItem(Some(h), id)
                    .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                    .unwrap_or(-1)
            };
            let pick = |values: &[i64], id: i32, fallback: i64| -> i64 {
                let i = combo_index(id);
                if i < 0 { fallback } else { *values.get(i as usize).unwrap_or(&fallback) }
            };

            let mut dict_names = Vec::new();
            if let Ok(list) = GetDlgItem(Some(h), ID_DICTS) {
                let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
                for i in 0..count.max(0) {
                    let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(i as usize)), None).0;
                    if len <= 0 {
                        continue;
                    }
                    let mut buf = vec![0u16; len as usize + 1];
                    SendMessageW(list, LB_GETTEXT, Some(WPARAM(i as usize)),
                                 Some(LPARAM(buf.as_mut_ptr() as isize)));
                    dict_names.push(String::from_utf16_lossy(&buf[..len as usize]));
                }
            }
            if dict_names.is_empty() {
                dict_names = template.dict_names.clone();
            }

            let theme = if combo_index(ID_THEME) == 1 { "light" } else { "dark" };
            let font = {
                let i = combo_index(ID_FONT);
                if i < 0 {
                    template.font.clone()
                } else {
                    self.fonts.get(i as usize).cloned().unwrap_or_else(|| template.font.clone())
                }
            };

            SettingsForm {
                mode: if checked(ID_MODE_HOLD) {
                    crate::config::TriggerMode::HoldShift
                } else {
                    crate::config::TriggerMode::Live
                },
                theme: theme.to_string(),
                font,
                max_height_percent: pick(&self.heights, ID_MAX_HEIGHT,
                                         template.max_height_percent as i64) as u8,
                summary_chars: pick(&self.summaries, ID_SUMMARY,
                                    template.summary_chars as i64) as usize,
                highlight_match: checked(ID_HIGHLIGHT),
                scroll_popup: checked(ID_SCROLL),
                exclude_from_capture: checked(ID_EXCLUDE),
                dict_names,
                max_ocr_passes: pick(&self.passes, ID_PASSES,
                                     template.max_ocr_passes as i64) as u8,
                show_scan_region: checked(ID_SHOW_SCAN),
            }
        }
    }
}

/// Fill a combo with `values`, selecting `current`.
unsafe fn fill_numeric(combo: HWND, values: &[i64], current: i64) {
    // SAFETY: `combo` is a live control created by the caller.
    unsafe {
        for (i, v) in values.iter().enumerate() {
            SendMessageW(combo, CB_ADDSTRING, None,
                         Some(LPARAM(wide(&v.to_string()).as_ptr() as isize)));
            if *v == current {
                SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(i)), None);
            }
        }
        if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        OUTCOME.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        // SAFETY: the window is this struct's own, still live, and destroyed
        // exactly once. The font outlives every control that used it because
        // the window (and therefore its children) is destroyed first.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if let Some(f) = self.font {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_choices_step_the_range() {
        assert_eq!(vec![10, 15, 20], numeric_choices(10, 20, 5, 10));
    }

    /// A hand-edited value off the step must be offered rather than snapped -
    /// opening Settings and applying must never change a setting the user did
    /// not touch.
    #[test]
    fn an_off_step_value_is_inserted_in_order() {
        assert_eq!(vec![10, 13, 15, 20], numeric_choices(10, 20, 5, 13));
    }

    #[test]
    fn a_value_outside_the_range_is_not_inserted() {
        assert_eq!(vec![10, 15, 20], numeric_choices(10, 20, 5, 999));
    }

    /// The vertical-writing duplicates Windows lists beside each family are
    /// never wanted, and the real list must not be empty on this machine.
    #[test]
    fn the_japanese_font_list_excludes_vertical_duplicates() {
        let families = japanese_font_families();
        assert!(!families.is_empty(), "no Japanese-capable font families found");
        assert!(!families.iter().any(|f| f.starts_with('@')), "got {families:?}");
    }
}
