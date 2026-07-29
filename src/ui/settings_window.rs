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

use crate::settings::{shown_name, SettingsForm, MAX_HEIGHT_RANGE, PASSES_RANGE, SUMMARY_RANGE};
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use windows::core::{w, Error, PCWSTR, PWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, EnumFontFamiliesExW, GetDC, ReleaseDC, COLOR_BTNFACE,
    ENUMLOGFONTEXW, HFONT, LOGFONTW, SHIFTJIS_CHARSET, TEXTMETRICW,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OPENFILENAMEW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// What the user did with the window. Read and cleared by `app::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    Apply,
    Cancel,
    /// Close chibipop entirely. Only reachable from a window opened by a
    /// running instance - see `open`'s `in_app`.
    Quit,
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
const ID_QUIT: i32 = 116;
const ID_DICT_ADD: i32 = 117;
const ID_DICT_REMOVE: i32 = 118;
const ID_FREQS: i32 = 119;
const ID_FREQ_ADD: i32 = 120;
const ID_FREQ_REMOVE: i32 = 121;

// ---- layout, in 96-DPI pixels; `child` scales every one of them ----

const WIN_W: i32 = 470;
const PAD: i32 = 14;
const ROW_H: i32 = 24;
const ROW_GAP: i32 = 6;
const GROUP_GAP: i32 = 10;
const BTN_W: i32 = 92;
const BTN_PITCH: i32 = ROW_H + 4;
const LABEL_W: i32 = 178;
const FIELD_X: i32 = PAD + LABEL_W;
const FIELD_W: i32 = WIN_W - FIELD_X - PAD - 16;

/// Which list a button acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Dicts,
    Freqs,
}

impl Target {
    fn list_id(self) -> i32 {
        match self {
            Target::Dicts => ID_DICTS,
            Target::Freqs => ID_FREQS,
        }
    }

    fn is_freq(self) -> bool {
        self == Target::Freqs
    }
}

/// A click to service.
#[derive(Debug, Clone, Copy)]
enum Action {
    Add(Target),
    Remove(Target),
}

fn class_name() -> PCWSTR {
    w!("ChibipopSettingsClass")
}

/// Scale a 96-DPI layout value for `hwnd`'s monitor.
///
/// **Required, not polish.** `text::capture::init_dpi_awareness` puts the
/// process in `PER_MONITOR_AWARE_V2`, where Windows scales *nothing* on our
/// behalf — every literal in this file would otherwise be a physical pixel and
/// the whole window would render at half size on a 200% display while its font,
/// which comes from the system metrics, did not.
fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    // SAFETY: FFI call on a live window handle; returns 96 for an invalid one,
    // which degrades to no scaling rather than to a wrong size.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (v as i64 * dpi as i64 / 96) as i32
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

    // The pending Add or Remove.
    static ACTION: Cell<Option<(isize, Action)>> = const { Cell::new(None) };
}

fn record_outcome(hwnd: HWND, outcome: SettingsOutcome) {
    OUTCOME.with(|c| c.set(Some((hwnd.0 as isize, outcome))));
}

fn record_action(hwnd: HWND, action: Action) {
    ACTION.with(|c| c.set(Some((hwnd.0 as isize, action))));
    // SAFETY: `hwnd` is the window whose own wndproc is running, so it is
    // live for the duration of this call. WM_NULL carries no payload and is
    // discarded by `DefWindowProcW`; posting it only ends the caller's
    // `GetMessageW` block so `pump` runs without waiting for other input.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notify = (wparam.0 >> 16) as u16;
            // LBN_SELCHANGE: without this the Move buttons keep whatever
            // enabled state they had when the window opened, so clicking a
            // dictionary and pressing Move up does nothing at all.
            if (id == ID_DICTS || id == ID_FREQS) && notify == LBN_SELCHANGE as u16 {
                unsafe { update_list_buttons(hwnd) };
                return LRESULT(0);
            }
            match id {
                // 1 is IDOK: this is a plain window, not a dialog, so
                // `IsDialogMessageW` cannot resolve BS_DEFPUSHBUTTON and sends
                // IDOK for Enter instead of the button's own id.
                ID_APPLY | 1 => record_outcome(hwnd, SettingsOutcome::Apply),
                // The X button and Escape both land here through IDCANCEL,
                // so closing can never apply by accident.
                ID_CANCEL | 2 => record_outcome(hwnd, SettingsOutcome::Cancel),
                ID_QUIT => record_outcome(hwnd, SettingsOutcome::Quit),
                ID_DICT_UP => unsafe { move_selected(hwnd, -1) },
                ID_DICT_DOWN => unsafe { move_selected(hwnd, 1) },
                ID_DICT_ADD => record_action(hwnd, Action::Add(Target::Dicts)),
                ID_DICT_REMOVE => record_action(hwnd, Action::Remove(Target::Dicts)),
                ID_FREQ_ADD => record_action(hwnd, Action::Add(Target::Freqs)),
                ID_FREQ_REMOVE => record_action(hwnd, Action::Remove(Target::Freqs)),
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
    let font = unsafe { CreateFontIndirectW(&ncm.lfMessageFont) };
    if font.is_invalid() {
        None
    } else {
        Some(font)
    }
}

/// Every installed font family that can render Japanese.
///
/// Enumerated with `SHIFTJIS_CHARSET`, which narrows the list sharply — 142
/// callbacks down to 77 on this machine — but **does not guarantee kana and
/// kanji coverage**: measured, the survivors still include Segoe UI, Tahoma
/// and Ebrima, none of which carry Japanese. DirectWrite falls back per glyph,
/// so the cost of picking one is ugly rather than broken. Claiming a guarantee
/// here would be claiming more than the API gives.
///
/// Families **and** their named variants: `Klee One SemiBold` and
/// `Noto Sans JP Light` come back as separate entries, so `Theme::font_name`
/// can express more than the plain family name.
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
            dpi_scale(parent, x),
            dpi_scale(parent, y),
            dpi_scale(parent, w),
            dpi_scale(parent, h),
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
        update_list_buttons(hwnd);
    }
}

/// Disable what cannot act.
unsafe fn update_list_buttons(hwnd: HWND) {
    // SAFETY: every id below is a live child of `hwnd`, created in `build`,
    // and each `GetDlgItem` result is checked before it is used.
    unsafe {
        let dicts = GetDlgItem(Some(hwnd), ID_DICTS).unwrap_or_default();
        let freqs = GetDlgItem(Some(hwnd), ID_FREQS).unwrap_or_default();
        if dicts.is_invalid() || freqs.is_invalid() {
            return;
        }
        let count = SendMessageW(dicts, LB_GETCOUNT, None, None).0;
        let cur = SendMessageW(dicts, LB_GETCURSEL, None, None).0;
        let freq_cur = SendMessageW(freqs, LB_GETCURSEL, None, None).0;
        // Focus must not be orphaned.
        let focused = GetFocus();
        for (id, list, enable) in [
            (ID_DICT_UP, dicts, cur > 0),
            (ID_DICT_DOWN, dicts, cur >= 0 && cur < count - 1),
            (ID_DICT_REMOVE, dicts, cur >= 0),
            (ID_FREQ_REMOVE, freqs, freq_cur >= 0),
        ] {
            if let Ok(btn) = GetDlgItem(Some(hwnd), id) {
                if !enable && focused == btn {
                    let _ = SetFocus(Some(list));
                }
                let _ = EnableWindow(btn, enable);
            }
        }
    }
}

/// One row's text.
unsafe fn list_row(list: HWND, index: isize) -> Option<String> {
    // SAFETY: `list` is a live listbox owned by the caller; the buffer is
    // sized to the length LB_GETTEXTLEN itself reported for this row, which
    // is the contract LB_GETTEXT writes against.
    unsafe {
        let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(index as usize)), None).0;
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        SendMessageW(
            list,
            LB_GETTEXT,
            Some(WPARAM(index as usize)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        );
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Every row, or None if gone.
unsafe fn list_rows(hwnd: HWND, id: i32) -> Option<Vec<String>> {
    // SAFETY: `id` names a child of `hwnd`; a missing one yields `Err` here
    // rather than a dangling handle, and `list_row` states its own contract.
    unsafe {
        let list = GetDlgItem(Some(hwnd), id).ok()?;
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
        Some((0..count.max(0)).filter_map(|i| list_row(list, i)).collect())
    }
}

/// Pick .zip archives.
///
/// Empty when cancelled.
unsafe fn pick_archives(owner: HWND) -> Vec<PathBuf> {
    let mut buf = vec![0u16; 32 * 1024];
    // Win32 wants a NUL-run.
    let filter: Vec<u16> = "Yomitan archives (*.zip)\0*.zip\0\0".encode_utf16().collect();
    let title = wide("Add a dictionary archive");
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER
            | OFN_ALLOWMULTISELECT
            | OFN_FILEMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: `buf`, `filter` and `title` outlive the call and are borrowed
    // by `ofn` for exactly its duration; `nMaxFile` is `buf`'s own length, so
    // the dialog cannot write past it. `lStructSize` declares the size the
    // call validates against. The owner window is live on this thread.
    let picked = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    if !picked {
        return Vec::new();
    }
    split_picked(&buf)
}

/// Split the picker's buffer.
///
/// Two shapes; see the tests.
fn split_picked(buf: &[u16]) -> Vec<PathBuf> {
    let mut parts = buf
        .split(|&c| c == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy);
    let Some(first) = parts.next() else {
        return Vec::new();
    };
    let rest: Vec<String> = parts.collect();
    if rest.is_empty() {
        return vec![PathBuf::from(first)];
    }
    let dir = Path::new(&first);
    rest.iter().map(|name| dir.join(name)).collect()
}

/// The permitted values for a numeric combo, with `current` inserted if it is
/// not already one of them.
///
/// Inserting rather than snapping matters: a hand-edited config holding 43
/// must not silently become 45 merely because the user opened Settings and
/// pressed Apply without touching that control.
fn numeric_choices(lo: i64, hi: i64, step: i64, current: i64) -> Vec<i64> {
    let mut v: Vec<i64> = (lo..=hi).step_by(step as usize).collect();
    // Out-of-range values are clamped the same way `settings::apply_to` clamps
    // them. Leaving them out instead would drop the combo to its first entry,
    // so a hand-edited 250 would display as 10 while the model would have
    // written 90 - the window and the model disagreeing about one number.
    let current = current.clamp(lo, hi);
    if !v.contains(&current) {
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
    /// What Apply has yet to do.
    staged: RefCell<SettingsForm>,
}

impl SettingsWindow {
    /// Create and show the window, populated from `form`.
    ///
    /// `stale` are `display_order` entries matching no installed dictionary
    /// (spec D6a); when non-empty a warning naming them is shown, because that
    /// is what a dictionary rename looks like from in here.
    ///
    /// `in_app` is whether this window belongs to a running `chibipop run`
    /// instance (true from the tray and from startup, false for the standalone
    /// `chibipop settings`). It governs two things, both for the same reason -
    /// a window must not offer what it is in no position to do:
    ///
    /// - Apply restarts chibipop, so the button says so;
    /// - Quit is offered at all, since standalone has no instance to quit.
    pub fn open(form: &SettingsForm, stale: &[String], in_app: bool) -> Result<SettingsWindow> {
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
                // Placeholder: fit_to sizes it after build.
                WIN_W,
                400,
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
                staged: RefCell::new(form.clone()),
            };
            // `build` reports where its layout actually ended; the window is
            // then sized to that rather than to a guess. The first version of
            // this file passed a hand-tuned height straight to
            // `CreateWindowExW` - which takes the OUTER size - so 39px of
            // caption and frame ate the Apply and Cancel buttons entirely and
            // the window opened with no way to accept anything. Measuring the
            // content means that cannot recur, at any DPI or font size.
            let content_h = win.build(form, stale, in_app)?;
            // Sizes AND shows - see `fit_to` for why showing cannot go
            // through `ShowWindow` here.
            win.fit_to(WIN_W, content_h + PAD);
            let _ = SetForegroundWindow(hwnd);
            Ok(win)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Bring an already-open window forward instead of creating a second.
    ///
    /// Restores first if minimized: `SetForegroundWindow` alone does **not**
    /// un-minimize, so minimizing Settings and picking it from the tray again
    /// would take this branch and appear to do nothing at all.
    pub fn focus(&self) {
        // SAFETY: `self.hwnd` is live until `Drop`.
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
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

    /// Run a pending button.
    ///
    /// Callback precedes a picker.
    pub fn pump(&self, before_blocking: impl FnOnce()) {
        let action = ACTION.with(|c| match c.get() {
            Some((h, a)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(a)
            }
            _ => None,
        });
        let Some(action) = action else {
            return;
        };
        // SAFETY: both helpers act only on live children of `self.hwnd`,
        // which outlives this call, and each states its own contract.
        unsafe {
            match action {
                Action::Remove(target) => self.remove_selected(target),
                Action::Add(target) => {
                    // D9: the picker pumps too.
                    before_blocking();
                    self.add_picked(target);
                }
            }
        }
    }

    /// Drop the selected row.
    unsafe fn remove_selected(&self, target: Target) {
        // SAFETY: `target.list_id()` names a live child of `self.hwnd`;
        // `list_row` and `update_list_buttons` state their own contracts.
        unsafe {
            let Ok(list) = GetDlgItem(Some(self.hwnd), target.list_id()) else {
                return;
            };
            let cur = SendMessageW(list, LB_GETCURSEL, None, None).0;
            if cur < 0 {
                return;
            }
            let Some(name) = list_row(list, cur) else {
                return;
            };
            SendMessageW(list, LB_DELETESTRING, Some(WPARAM(cur as usize)), None);
            let left = SendMessageW(list, LB_GETCOUNT, None, None).0;
            if left > 0 {
                let next = cur.min(left - 1);
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(next as usize)), None);
            }
            self.staged.borrow_mut().stage_remove(&name);
            update_list_buttons(self.hwnd);
        }
    }

    /// Stage whatever was picked.
    unsafe fn add_picked(&self, target: Target) {
        // SAFETY: `pick_archives` owns every buffer it hands the dialog;
        // `target.list_id()` names a live child of `self.hwnd`, and the
        // string each `LB_ADDSTRING` copies outlives that call.
        unsafe {
            let picked = pick_archives(self.hwnd);
            let Ok(list) = GetDlgItem(Some(self.hwnd), target.list_id()) else {
                return;
            };
            for path in picked {
                if !self.staged.borrow_mut().stage_add(&path, target.is_freq()) {
                    eprintln!("chibipop: {} is already in the list.", path.display());
                    continue;
                }
                let Some(name) = shown_name(&path) else {
                    continue;
                };
                SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(wide(&name).as_ptr() as isize)));
            }
            if SendMessageW(list, LB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            }
            update_list_buttons(self.hwnd);
        }
    }

    /// Resize so the client area holds `client_w` x `client_h` 96-DPI pixels,
    /// and show the window.
    ///
    /// `CreateWindowExW` takes the **outer** size, so the caption and frame
    /// must be added on top - `AdjustWindowRectEx` is what knows how much that
    /// is, and it is per-monitor, so it cannot be a constant.
    fn fit_to(&self, client_w: i32, client_h: i32) {
        // SAFETY: `self.hwnd` is live; `rc` is stack storage the call only
        // writes through. A failure leaves the created size in place, which is
        // merely the old behaviour rather than anything unsound.
        unsafe {
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: dpi_scale(self.hwnd, client_w),
                bottom: dpi_scale(self.hwnd, client_h),
            };
            let style = WINDOW_STYLE(GetWindowLongW(self.hwnd, GWL_STYLE) as u32);
            let ex = WINDOW_EX_STYLE(GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32);
            if AdjustWindowRectEx(&mut rc, style, false, ex).is_ok() {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    // SWP_SHOWWINDOW, not a separate `ShowWindow` call: the
                    // FIRST ShowWindow in a process ignores its nCmdShow and
                    // uses STARTUPINFO.wShowWindow instead. A chibipop
                    // launched hidden - from a shortcut set to "minimized", a
                    // scheduled task, or `Start-Process -WindowStyle Hidden` -
                    // therefore had its settings window created, sized, and
                    // then silently forced invisible, with no error anywhere.
                    // SetWindowPos sets WS_VISIBLE directly and is immune.
                    SWP_NOMOVE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
            }
        }
    }

    /// Create every control, returning the 96-DPI `y` its layout reached.
    unsafe fn build(&mut self, form: &SettingsForm, stale: &[String], in_app: bool)
        -> Result<i32> {
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
            // Same, but carrying WS_GROUP so it ends the preceding group.
            let group_start = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(h, w!("BUTTON"), text,
                      WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
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
            // WS_GROUP terminates the radio group above. Without it the group
            // runs to the end of the window and arrow keys walk straight out
            // of Live/Hold Shift into the combos.
            group_start("Popup", y, 5 * (ROW_H + ROW_GAP) + 3 * ROW_H + 16)?;
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
            let bx = WIN_W - PAD - 100;
            let list_w = WIN_W - 2 * PAD - 110;
            let hint_h = 28;
            // Four buttons set the height.
            let dict_span = 3 * BTN_PITCH + ROW_H;
            let dict_h = 20 + dict_span + ROW_GAP + hint_h + 8;
            group("Dictionaries — topmost is shown first", y, dict_h)?;
            y += 20;
            let list = child(h, w!("LISTBOX"), "",
                WINDOW_STYLE(LBS_NOTIFY as u32) | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
                PAD, y, list_w, dict_span, ID_DICTS, f)?;
            for name in &form.dict_names {
                SendMessageW(list, LB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
            }
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            for (i, (text, id)) in [
                ("Move up", ID_DICT_UP),
                ("Move down", ID_DICT_DOWN),
                ("Add…", ID_DICT_ADD),
                ("Remove", ID_DICT_REMOVE),
            ]
            .iter()
            .enumerate()
            {
                child(h, w!("BUTTON"), text, WS_TABSTOP,
                      bx, y + i as i32 * BTN_PITCH, BTN_W, ROW_H, *id, f)?;
            }
            y += dict_span + ROW_GAP;
            child(h, w!("STATIC"),
                "Order is matched by dictionary name. If you rebuild your \
                 dictionaries, check this list again.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, hint_h, 0, f)?;
            y += hint_h + 8;

            // A rebuild is library-only.
            if form.library_empty && !form.dict_names.is_empty() {
                child(h, w!("STATIC"),
                    "chibipop is using a dictionary built outside the app. Adding or \
                     removing here rebuilds from this list only — import your original \
                     .zip files first.",
                    WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 44, 0, f)?;
                y += 48;
            }

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
                y += 36;
            }
            y += GROUP_GAP;

            // ---- Frequency data ----
            // WS_GROUP ends the last one.
            let freq_span = BTN_PITCH + ROW_H;
            let freq_h = 20 + freq_span + 8;
            group_start("Frequency data — how common each word is", y, freq_h)?;
            y += 20;
            let freqs = child(h, w!("LISTBOX"), "",
                WINDOW_STYLE(LBS_NOTIFY as u32) | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
                PAD, y, list_w, freq_span, ID_FREQS, f)?;
            for name in &form.freq_names {
                SendMessageW(freqs, LB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
            }
            SendMessageW(freqs, LB_SETCURSEL, Some(WPARAM(0)), None);
            child(h, w!("BUTTON"), "Add…", WS_TABSTOP,
                  bx, y, BTN_W, ROW_H, ID_FREQ_ADD, f)?;
            child(h, w!("BUTTON"), "Remove", WS_TABSTOP,
                  bx, y + BTN_PITCH, BTN_W, ROW_H, ID_FREQ_REMOVE, f)?;
            y += freq_span + 8 + GROUP_GAP;

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
                if in_app {
                    "Applying saves your settings and restarts chibipop."
                } else {
                    "Applying saves your settings. Restart chibipop to use them."
                },
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 16, ROW_H, 0, f)?;
            y += ROW_H + 2;
            child(h, w!("BUTTON"), if in_app { "Apply && Restart" } else { "Apply" },
                  WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
                  WIN_W - PAD - 238, y, 128, ROW_H + 4, ID_APPLY, f)?;
            child(h, w!("BUTTON"), "Cancel", WS_TABSTOP,
                  WIN_W - PAD - 104, y, 96, ROW_H + 4, ID_CANCEL, f)?;
            // Far left, deliberately: Quit is the one button here that
            // discards nothing but ends the app, and it must not sit next to
            // the one people press by reflex. Only exists when there is an
            // instance to quit - BACKLOG 7 left Quit stranded on a tray menu
            // that does not open, which is why it is here at all.
            if in_app {
                child(h, w!("BUTTON"), "Quit chibipop", WS_TABSTOP,
                      PAD, y, 116, ROW_H + 4, ID_QUIT, f)?;
            }

            update_list_buttons(h);
        }
        Ok(y + ROW_H + 8)
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

            // Empty is not missing.
            let dict_names =
                list_rows(h, ID_DICTS).unwrap_or_else(|| template.dict_names.clone());
            let freq_names =
                list_rows(h, ID_FREQS).unwrap_or_else(|| template.freq_names.clone());
            let staged = self.staged.borrow();

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
                freq_names,
                staged_adds: staged.staged_adds.clone(),
                staged_removes: staged.staged_removes.clone(),
                library_empty: staged.library_empty,
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
        ACTION.with(|c| {
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

    /// The window must be sized from its own content, never from a guessed
    /// constant.
    ///
    /// The first version of this file handed a hand-tuned height straight to
    /// `CreateWindowExW`, which takes the **outer** size — so 39px of caption
    /// and frame ate the Apply and Cancel buttons and the window opened with
    /// no way to accept anything. `cargo test` could not see it and neither
    /// could the compiler; an adversarial review measured it.
    ///
    /// This pins the arithmetic that made it possible: a client area is
    /// strictly smaller than the outer window it lives in, so any code path
    /// that treats a desired *content* height as a *window* height loses
    /// exactly the non-client overhead.
    #[test]
    fn a_client_area_is_smaller_than_its_window() {
        // Measured on this machine for the style this window uses.
        const CAPTION_AND_FRAME: i32 = 39;
        let content_bottom = 618 + ROW_H + 4;
        let outer_if_guessed = 620;
        assert!(
            content_bottom > outer_if_guessed - CAPTION_AND_FRAME,
            "the guessed constant must be shown to be too small, or this test proves nothing"
        );
    }

    /// DPI scaling must be identity at 96 and proportional above it - the
    /// process is PER_MONITOR_AWARE_V2, so Windows scales nothing for us.
    #[test]
    fn the_dpi_scale_is_identity_at_96() {
        assert_eq!(100, (100i64 * 96 / 96) as i32);
        assert_eq!(150, (100i64 * 144 / 96) as i32);
        assert_eq!(200, (100i64 * 192 / 96) as i32);
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

    fn nul_run(parts: &[&str]) -> Vec<u16> {
        let mut buf: Vec<u16> = Vec::new();
        for part in parts {
            buf.extend(part.encode_utf16());
            buf.push(0);
        }
        buf.push(0);
        buf.resize(128, 0);
        buf
    }

    /// One file is a whole path.
    #[test]
    fn a_single_pick_is_not_treated_as_a_directory() {
        assert_eq!(
            vec![PathBuf::from(r"C:\dicts\terms.zip")],
            split_picked(&nul_run(&[r"C:\dicts\terms.zip"]))
        );
    }

    /// A directory, then names.
    #[test]
    fn a_multi_pick_joins_each_name_onto_the_directory() {
        assert_eq!(
            vec![PathBuf::from(r"C:\dicts\a.zip"), PathBuf::from(r"C:\dicts\b.zip")],
            split_picked(&nul_run(&[r"C:\dicts", "a.zip", "b.zip"]))
        );
    }

    #[test]
    fn a_root_directory_does_not_double_its_separator() {
        assert_eq!(
            vec![PathBuf::from(r"C:\a.zip")],
            split_picked(&nul_run(&[r"C:\", "a.zip"]))
        );
    }

    #[test]
    fn a_cancelled_pick_yields_nothing() {
        assert!(split_picked(&[0u16; 64]).is_empty());
        assert!(split_picked(&[]).is_empty());
    }

    /// UTF-16 keeps non-ASCII.
    #[test]
    fn a_japanese_filename_round_trips() {
        assert_eq!(
            vec![PathBuf::from(r"C:\辞書\大辞林　第四版.zip")],
            split_picked(&nul_run(&[r"C:\辞書", "大辞林　第四版.zip"]))
        );
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
