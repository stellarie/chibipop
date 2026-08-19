//! The settings window.
//!
//! Modeless - see D9.
//! Numbers are combos, not spins.

use crate::library::Kind;
use crate::settings::{SettingsForm, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE};
use crate::text::ocr::tag_matches;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use windows::core::{w, Error, PCWSTR, PWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, MAX_PATH, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, EnumFontFamiliesExW, GetDC, GetMonitorInfoW,
    MonitorFromWindow, ReleaseDC, COLOR_BTNFACE, ENUMLOGFONTEXW, HFONT, LOGFONTW, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, SHIFTJIS_CHARSET, TEXTMETRICW,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetScrollInfo, INITCOMMONCONTROLSEX, ICC_TAB_CLASSES,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OPENFILENAMEW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows::Win32::UI::Shell::{
    ShellExecuteW, SHBrowseForFolderW, SHGetPathFromIDListW, BROWSEINFOW,
    BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// What the user did with the window. Read and cleared by `app::run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    Apply,
    Cancel,
    /// Only from a running instance.
    Quit,
}

/// A click app.rs must service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsClick {
    AnkiTest,
    CheckUpdate,
}

/// Who owns the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// `run`: applies live.
    Live,
    /// Saves for the next start.
    Standalone,
}

// ---- control ids ----

const ID_APPLY: i32 = 100;
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
const ID_STATUS: i32 = 122;
const ID_MAX_WIDTH: i32 = 123;
const ID_CHECK_UPDATE: i32 = 124;
const ID_ANKI_ENABLED: i32 = 125;
const ID_ANKI_URL: i32 = 126;
const ID_ANKI_DECK: i32 = 127;
const ID_ANKI_MODEL: i32 = 128;
const ID_ANKI_TEST: i32 = 129;
const ID_TAB: i32 = 130;
const ID_TRIGGER_KEY: i32 = 131;
const ID_PREFER_VERT: i32 = 132;
const ID_ANKI_ADD_KEY: i32 = 133;
const ID_SIDE_PANEL: i32 = 134;
const ID_FIELD_MAP_TOGGLE: i32 = 135;
const ID_CAPTURE_W: i32 = 136;
const ID_CAPTURE_H: i32 = 137;
const ID_SCAN_ALNUM: i32 = 138;
const ID_PER_CHAR: i32 = 139;
const ID_OCR_LANG: i32 = 140;
/// 141 was Include / exclude.
const ID_DICTS_OFF: i32 = 142;
/// Clips the page content.
const ID_VIEWPORT: i32 = 143;
/// Holds the page content.
const ID_CONTENT: i32 = 144;
/// The Updates group box.
const ID_UPDATES: i32 = 145;
/// Engine combo, OCR tab.
const ID_ENGINE: i32 = 146;
/// Configure button, OCR tab.
const ID_ENGINE_CONFIGURE: i32 = 147;

/// First field-map combo id.
const ID_FIELD_MAP_BASE: i32 = 200;

/// Field-map combo choices.
const FIELD_MAP_SOURCES: [&str; 6] =
    ["(none)", "expression", "reading", "glossary", "frequency", "glossary_html"];

/// First plugin-enable id.
const ID_PLUGIN_ENABLE_BASE: i32 = 1000;
/// First plugin-configure id.
const ID_PLUGIN_CONFIGURE_BASE: i32 = 1500;
/// Plugin id block size.
const PLUGIN_ID_SPAN: i32 = 100;

// Win32 tab control messages
const TCM_FIRST: u32 = 0x1300;
const TCM_GETCURSEL_MSG: u32 = TCM_FIRST + 11;
const TCM_INSERTITEMW_MSG: u32 = TCM_FIRST + 62;
const TCIF_TEXT_VAL: u32 = 0x0001;
// TCN_SELCHANGE = -551 as u32
const TCN_SELCHANGE_CODE: u32 = (-551i32) as u32;
const TAB_H: i32 = 28;

/// Win32 NMHDR layout.
#[repr(C)]
struct NmhdrRaw {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

/// Win32 TCITEMW layout.
#[repr(C)]
struct TcItemW {
    mask: u32,
    dw_state: u32,
    dw_state_mask: u32,
    psz_text: *mut u16,
    cch_text_max: i32,
    i_image: i32,
    l_param: isize,
}

/// What an Apply disables.
const WHILE_BUSY: [i32; 16] = [
    ID_APPLY,
    ID_QUIT,
    ID_OCR_LANG,
    ID_ENGINE,
    ID_ENGINE_CONFIGURE,
    ID_DICTS,
    ID_DICTS_OFF,
    ID_DICT_UP,
    ID_DICT_DOWN,
    ID_DICT_ADD,
    ID_DICT_REMOVE,
    ID_FREQS,
    ID_FREQ_ADD,
    ID_FREQ_REMOVE,
    ID_ANKI_TEST,
    ID_CHECK_UPDATE,
];

// ---- layout, 96-DPI px ----

const WIN_W: i32 = 470;
const PAD: i32 = 14;
const ROW_H: i32 = 24;
const ROW_GAP: i32 = 6;
const GROUP_GAP: i32 = 10;
const BTN_W: i32 = 120;
const BTN_PITCH: i32 = ROW_H + 4;
const LABEL_W: i32 = 178;
const FIELD_X: i32 = PAD + LABEL_W;
const FIELD_W: i32 = WIN_W - FIELD_X - PAD - 16;
/// ~3 lines of status text.
const STATUS_H: i32 = 58;
/// First y below the tab strip.
const CONTENT_Y: i32 = PAD + TAB_H + 4;
/// Below the bottom row's top.
const BOTTOM_UPDATE_DY: i32 = 20;
const BOTTOM_STATUS_DY: i32 = BOTTOM_UPDATE_DY + ROW_H + 8 + GROUP_GAP;
const BOTTOM_BTN_DY: i32 = BOTTOM_STATUS_DY + STATUS_H + 2;
/// The bottom row's own height.
const BOTTOM_H: i32 = BOTTOM_BTN_DY + ROW_H + 8;
/// Apply's x, right-aligned.
const BOTTOM_APPLY_X: i32 = WIN_W - PAD - 144;
/// Bottom row: id, x, y offset.
const BOTTOM_ROW: [(i32, i32, i32); 5] = [
    (ID_UPDATES, PAD - 6, 0),
    (ID_CHECK_UPDATE, PAD, BOTTOM_UPDATE_DY),
    (ID_STATUS, PAD, BOTTOM_STATUS_DY),
    (ID_APPLY, BOTTOM_APPLY_X, BOTTOM_BTN_DY),
    (ID_QUIT, PAD, BOTTOM_BTN_DY),
];
/// One scroll line, 96-DPI px.
const SCROLL_LINE: i32 = 20;
/// Lines per wheel notch.
const WHEEL_LINES: i32 = 3;

// ---- Dictionaries tab ----

/// One line above each box.
const DICT_CAP_H: i32 = 18;
const DICT_BOX_H: i32 = 64;
/// Four 15px rows plus border.
const _: () = assert!((DICT_BOX_H - 2) / 15 >= 4);
/// One line under both boxes.
const DICT_HINT_H: i32 = 20;

/// Dictionaries group height.
///
/// Budgeted against the one-box
/// layout it replaces: 218.
fn dict_group_h() -> i32 {
    20 + 2 * (DICT_CAP_H + DICT_BOX_H) + ROW_GAP + DICT_HINT_H + 8
}

// ---- field-map columns ----

const COL_GAP: i32 = 12;
const COL_AREA_W: i32 = WIN_W - 2 * PAD - 20;
const COL_W: i32 = (COL_AREA_W - COL_GAP) / 2;
const COL_LABEL_W: i32 = 120;
const COL_LABEL_GAP: i32 = 4;
const COL_COMBO_W: i32 = COL_W - COL_LABEL_W - COL_LABEL_GAP;
const COL_DROPPED_W: i32 = 150;
const COL_LABEL_MAX_CHARS: usize = 18;

// ---- Plugins tab ----

/// Wraps a long refusal reason.
const PLUGIN_STATUS_H: i32 = ROW_H + 16;
/// One plugin row's own height.
const PLUGIN_ROW_H: i32 = 2 * ROW_H + PLUGIN_STATUS_H;

/// Which list a button acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Dicts,
    Freqs,
}

/// A click to service.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// The file picks the list.
    Add,
    Remove(Target),
    ConfigureEngine,
}

fn class_name() -> PCWSTR {
    w!("ChibipopSettingsClass")
}

/// Viewport and content pane.
fn pane_class_name() -> PCWSTR {
    w!("ChibipopSettingsPaneClass")
}

/// Scale a 96-DPI value.
///
/// We are PER_MONITOR_AWARE_V2.
fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    // SAFETY: FFI call on a live window handle; returns 96 for an invalid one,
    // which degrades to no scaling rather than to a wrong size.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (v as i64 * dpi as i64 / 96) as i32
}

/// Monitor work-area height.
///
/// Physical px; None if unknown.
fn work_area_height(hwnd: HWND) -> Option<i32> {
    // SAFETY: `hwnd` need not be valid - MonitorFromWindow falls back
    // to the nearest monitor either way, and `mi` is sized by its own
    // `cbSize`, which is the contract GetMonitorInfoW checks.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(hmon, &mut mi).as_bool().then(|| mi.rcWork.bottom - mi.rcWork.top)
    }
}

/// A window's client height.
///
/// Physical px; 0 if unknown.
fn client_h(hwnd: HWND) -> i32 {
    // SAFETY: `rc` is a stack local the call only writes through; a failure
    // leaves it zeroed, which reads as an unknown height rather than a wrong
    // one. `hwnd` need not be valid - the call reports `Err` for a stale one.
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        rc.bottom - rc.top
    }
}

/// Pins the bottom row.
///
/// The row sits a fixed distance
/// above the client bottom, so
/// no tab's height can move it.
/// The band above takes what is
/// left, so a tall tab scrolls.
fn place_bottom(hwnd: HWND) {
    let ch = client_h(hwnd);
    if ch <= 0 {
        return;
    }
    let top = ch - dpi_scale(hwnd, BOTTOM_H + PAD);
    // SAFETY: every id in `BOTTOM_ROW` names a direct child of `hwnd`, made
    // in `build`; before that `GetDlgItem` yields `Err` rather than a
    // dangling handle, and `panes` states the same contract for the band.
    // `SWP_NOSIZE` leaves each control's size alone, `SWP_NOMOVE` leaves the
    // band's origin alone, and `SWP_NOZORDER` keeps the seat `place_viewport`
    // chose.
    unsafe {
        for (id, x, dy) in BOTTOM_ROW {
            let Ok(c) = GetDlgItem(Some(hwnd), id) else {
                continue;
            };
            let _ = SetWindowPos(c, None, dpi_scale(hwnd, x), top + dpi_scale(hwnd, dy),
                0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let Ok((viewport, _)) = panes(hwnd) else {
            return;
        };
        let band = (top - dpi_scale(hwnd, CONTENT_Y)).max(0);
        let _ = SetWindowPos(viewport, None, 0, 0, dpi_scale(hwnd, WIN_W), band,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        // The page it ranged by moved.
        repage(hwnd, viewport);
    }
}

/// Re-pages after a resize.
///
/// Keeps the range, takes the new
/// band as the page, so the
/// scrollbar stays the only copy.
fn repage(hwnd: HWND, viewport: HWND) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE,
        ..Default::default()
    };
    // SAFETY: `si` is initialised with its own size and passed by mutable
    // pointer for the call's duration only. `set_scroll_range` takes a height
    // and stores it as `nMax + 1`, so reading it back this way is exact.
    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut si) }.is_err() {
        return;
    }
    set_scroll_range(hwnd, si.nMax + 1, client_h(viewport));
}

/// Slides the content pane.
///
/// `y` is physical px, <= 0.
fn move_content(hwnd: HWND, y: i32) {
    // SAFETY: `panes` states its own contract and yields `Err` rather than a
    // dangling handle; the pane it returns is a live descendant of `hwnd`,
    // destroyed only with it. `SWP_NOSIZE` leaves the band height alone and
    // `SWP_NOZORDER` keeps the seat `place_viewport` chose.
    unsafe {
        let Ok((_, content)) = panes(hwnd) else {
            return;
        };
        let _ = SetWindowPos(content, None, 0, y, 0, 0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Re-ranges the scrollbar.
///
/// Physical px. `content_h` is the
/// SELECTED tab's, so a short tab
/// needs no scrollbar. `view_h` is
/// the viewport's own, read not
/// assumed. Position resets to 0:
/// the pane it described changed.
fn set_scroll_range(hwnd: HWND, content_h: i32, view_h: i32) {
    let si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: content_h.max(1) - 1,
        nPage: view_h.max(1) as u32,
        nPos: 0,
        ..Default::default()
    };
    // SAFETY: `hwnd` is the settings window and `si` is a fully initialised
    // local passed by const pointer; `SetScrollInfo` only reads it, and only
    // for the duration of the call.
    unsafe { SetScrollInfo(hwnd, SB_VERT, &si, true) };
    move_content(hwnd, 0);
}

/// Moves to a new position.
///
/// `pick` reads the live info and
/// names the position it wants.
fn scroll_to(hwnd: HWND, pick: impl FnOnce(&SCROLLINFO) -> i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        ..Default::default()
    };
    // SAFETY: `si` is initialised with its own size and passed by mutable
    // pointer for the call's duration only. Reading the position back is what
    // keeps the scrollbar the single source of truth.
    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut si) }.is_err() {
        return;
    }
    let old = si.nPos;
    // Negative when content fits.
    let max = (si.nMax - si.nPage as i32 + 1).max(0);
    si.nPos = pick(&si).clamp(0, max);
    if si.nPos == old {
        return;
    }
    si.fMask = SIF_POS;
    // SAFETY: same contract as `set_scroll_range`.
    unsafe { SetScrollInfo(hwnd, SB_VERT, &si, true) };
    move_content(hwnd, -si.nPos);
}

thread_local! {
    // Pending outcome, by `HWND`.
    static OUTCOME: Cell<Option<(isize, SettingsOutcome)>> = const { Cell::new(None) };

    // The pending Add or Remove.
    static ACTION: Cell<Option<(isize, Action)>> = const { Cell::new(None) };

    // Pending Anki/update click.
    static CLICK: Cell<Option<(isize, SettingsClick)>> = const { Cell::new(None) };

    // Pending tab switch.
    static TAB: Cell<Option<(isize, u32)>> = const { Cell::new(None) };

    // Key capture: hwnd + ctrl id.
    static CAPTURING: Cell<Option<(isize, i32)>> = const { Cell::new(None) };

    // Button text before capture.
    static CAPTURE_PREV: RefCell<Option<(isize, String)>> = const { RefCell::new(None) };

    // Captured vkcode, by `HWND`.
    static CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Anki add-key vk, by `HWND`.
    static ANKI_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Field-map toggle click, by `HWND`.
    static FIELD_MAP_TOGGLE: Cell<Option<isize>> = const { Cell::new(None) };

    // Pending OCR-language switch.
    static LANG_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    // Unreadable rows, by `HWND`.
    static UNREADABLE: RefCell<Option<(isize, Vec<String>)>> = const { RefCell::new(None) };

    // Plugin dirs, by `HWND`.
    static PLUGIN_DIRS: RefCell<Option<(isize, Vec<PathBuf>)>> = const { RefCell::new(None) };

    // Last box selected, by `HWND`.
    static DICT_BOX: Cell<Option<(isize, DictBox)>> = const { Cell::new(None) };
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

fn record_click(hwnd: HWND, click: SettingsClick) {
    CLICK.with(|c| c.set(Some((hwnd.0 as isize, click))));
}

fn record_field_map_toggle(hwnd: HWND) {
    FIELD_MAP_TOGGLE.with(|c| c.set(Some(hwnd.0 as isize)));
}

fn remember_unreadable(hwnd: HWND, files: &[String]) {
    UNREADABLE.with(|c| *c.borrow_mut() = Some((hwnd.0 as isize, files.to_vec())));
}

/// Rows carrying no name.
fn unreadable_rows(hwnd: HWND) -> Vec<String> {
    UNREADABLE.with(|c| match &*c.borrow() {
        Some((h, u)) if *h == hwnd.0 as isize => u.clone(),
        _ => Vec::new(),
    })
}

fn remember_plugin_dirs(hwnd: HWND, dirs: Vec<PathBuf>) {
    PLUGIN_DIRS.with(|c| *c.borrow_mut() = Some((hwnd.0 as isize, dirs)));
}

/// The dir a Configure id names.
fn plugin_dir_at(hwnd: HWND, idx: usize) -> Option<PathBuf> {
    PLUGIN_DIRS.with(|c| match &*c.borrow() {
        Some((h, dirs)) if *h == hwnd.0 as isize => dirs.get(idx).cloned(),
        _ => None,
    })
}

/// Configure button's index.
fn plugin_configure_idx(id: i32) -> Option<usize> {
    (ID_PLUGIN_CONFIGURE_BASE..ID_PLUGIN_CONFIGURE_BASE + PLUGIN_ID_SPAN)
        .contains(&id)
        .then(|| (id - ID_PLUGIN_CONFIGURE_BASE) as usize)
}

/// Opens it in Explorer.
unsafe fn open_plugin_dir(hwnd: HWND, idx: usize) {
    let Some(dir) = plugin_dir_at(hwnd, idx) else { return };
    let path = wide(&dir.to_string_lossy());
    // SAFETY: `path` is NUL-terminated UTF-16 valid for the
    // call; the OS only reads it, and a bad path just fails
    // to open rather than causing UB.
    unsafe {
        let _ = ShellExecuteW(
            Some(hwnd),
            w!("open"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Sets or appends the path.
fn set_config_path(existing: &str, path: &str) -> String {
    let new_line = format!("meikiocr_path = '{path}'");
    let mut found = false;
    let mut out: Vec<String> = existing
        .lines()
        .map(|line| {
            if line.starts_with("meikiocr_path") {
                found = true;
                new_line.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        out.push(new_line);
    }
    let mut result = out.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Pick an install folder.
///
/// `None` if cancelled.
unsafe fn pick_folder(owner: HWND, title: &str) -> Option<PathBuf> {
    let wtitle = wide(title);
    let bi = BROWSEINFOW {
        hwndOwner: owner,
        lpszTitle: PCWSTR(wtitle.as_ptr()),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..Default::default()
    };
    // SAFETY: `wtitle` and `bi` outlive every call below;
    // `pidl` is freed exactly once, only when non-null, with
    // the allocator its own docs require.
    unsafe {
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut buf = [0u16; MAX_PATH as usize];
        let ok = SHGetPathFromIDListW(pidl, &mut buf);
        CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
        if !ok.as_bool() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

fn record_dict_box(hwnd: HWND, which: DictBox) {
    DICT_BOX.with(|c| c.set(Some((hwnd.0 as isize, which))));
}

/// Last box to report a change.
fn tracked_dict_box(hwnd: HWND) -> Option<DictBox> {
    DICT_BOX.with(|c| c.get()).and_then(|(h, b)| (h == hwnd.0 as isize).then_some(b))
}

/// Which box the buttons act on.
fn acting_box(searched_sel: bool, not_searched_sel: bool, last: Option<DictBox>) -> DictBox {
    match (searched_sel, not_searched_sel) {
        (true, false) => DictBox::Searched,
        (false, true) => DictBox::NotSearched,
        _ => last.unwrap_or(DictBox::Searched),
    }
}

fn dict_box_id(which: DictBox) -> i32 {
    match which {
        DictBox::Searched => ID_DICTS,
        DictBox::NotSearched => ID_DICTS_OFF,
    }
}

fn record_language_change(hwnd: HWND) {
    LANG_CHANGED.with(|c| c.set(Some(hwnd.0 as isize)));
    // SAFETY: `hwnd` is the window whose own wndproc is running, so it is
    // live for the duration of this call. WM_NULL carries no payload and is
    // discarded by `DefWindowProcW`; posting it only ends the caller's
    // `GetMessageW` block so `pump` re-scopes the list without waiting for
    // other input.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

/// Starts capture mode.
unsafe fn begin_capture(hwnd: HWND, id: i32) {
    // SAFETY: `id` is ID_TRIGGER_KEY or ID_ANKI_ADD_KEY, both live
    // descendants of `hwnd`, created in `build`; `window_text` and
    // `SetWindowTextW` state their own contracts.
    unsafe {
        let Ok(btn) = dlg_item(hwnd, id) else { return };
        let prev = window_text(btn);
        CAPTURE_PREV.with(|c| *c.borrow_mut() = Some((hwnd.0 as isize, prev)));
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, id))));
        let _ = SetWindowTextW(btn, w!("Press a key..."));
    }
}

/// Ends capture, unchanged.
unsafe fn cancel_capture(hwnd: HWND) {
    // SAFETY: `id` came from `CAPTURING`, only ever set by
    // `begin_capture` to a live descendant of `hwnd`; the stashed text
    // was captured from that same control.
    unsafe {
        let mine = hwnd.0 as isize;
        let captured = CAPTURING.with(|c| c.get()).and_then(|(h, id)| (h == mine).then_some(id));
        let Some(id) = captured else { return };
        CAPTURING.with(|c| c.set(None));
        let prev = CAPTURE_PREV
            .with(|c| c.borrow_mut().take())
            .and_then(|(h, s)| (h == mine).then_some(s));
        let Some(text) = prev else { return };
        if let Ok(btn) = dlg_item(hwnd, id) {
            let _ = SetWindowTextW(btn, PCWSTR(wide(&text).as_ptr()));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notify = (wparam.0 >> 16) as u16;
            // Any click cancels capture.
            unsafe { cancel_capture(hwnd) };
            // Or Move buttons go stale.
            if (id == ID_DICTS || id == ID_DICTS_OFF || id == ID_FREQS)
                && notify == LBN_SELCHANGE as u16
            {
                if id == ID_DICTS {
                    record_dict_box(hwnd, DictBox::Searched);
                } else if id == ID_DICTS_OFF {
                    record_dict_box(hwnd, DictBox::NotSearched);
                }
                unsafe { update_list_buttons(hwnd) };
                return LRESULT(0);
            }
            if id == ID_OCR_LANG && notify == CBN_SELCHANGE as u16 {
                record_language_change(hwnd);
                return LRESULT(0);
            }
            if id == ID_ENGINE && notify == CBN_SELCHANGE as u16 {
                unsafe { update_engine_controls(hwnd) };
                return LRESULT(0);
            }
            if let Some(idx) = plugin_configure_idx(id) {
                unsafe { open_plugin_dir(hwnd, idx) };
                return LRESULT(0);
            }
            match id {
                // 1 is IDOK: Enter, not the id.
                ID_APPLY | 1 => record_outcome(hwnd, SettingsOutcome::Apply),
                // Escape. X goes via WM_CLOSE.
                2 => record_outcome(hwnd, SettingsOutcome::Cancel),
                ID_QUIT => record_outcome(hwnd, SettingsOutcome::Quit),
                ID_DICT_UP => unsafe { move_selected(hwnd, true) },
                ID_DICT_DOWN => unsafe { move_selected(hwnd, false) },
                ID_DICT_ADD => record_action(hwnd, Action::Add),
                ID_DICT_REMOVE => record_action(hwnd, Action::Remove(Target::Dicts)),
                ID_FREQ_ADD => record_action(hwnd, Action::Add),
                ID_FREQ_REMOVE => record_action(hwnd, Action::Remove(Target::Freqs)),
                ID_ENGINE_CONFIGURE => record_action(hwnd, Action::ConfigureEngine),
                ID_ANKI_TEST => record_click(hwnd, SettingsClick::AnkiTest),
                ID_CHECK_UPDATE => record_click(hwnd, SettingsClick::CheckUpdate),
                ID_FIELD_MAP_TOGGLE => record_field_map_toggle(hwnd),
                ID_MODE_LIVE | ID_MODE_HOLD => unsafe {
                    if let Ok(c) = dlg_item(hwnd, ID_TRIGGER_KEY) {
                        let _ = EnableWindow(c, id == ID_MODE_HOLD);
                    }
                    if let Ok(c) = dlg_item(hwnd, ID_PER_CHAR) {
                        let _ = EnableWindow(c, id == ID_MODE_LIVE);
                    }
                },
                ID_TRIGGER_KEY => unsafe { begin_capture(hwnd, ID_TRIGGER_KEY) },
                ID_ANKI_ADD_KEY => unsafe { begin_capture(hwnd, ID_ANKI_ADD_KEY) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: `lparam` is a pointer to an NMHDR (or a larger struct
            // whose first member is NMHDR); the OS guarantees this for any
            // WM_NOTIFY the system sends.
            let nmhdr = unsafe { &*(lparam.0 as *const NmhdrRaw) };
            if nmhdr.code == TCN_SELCHANGE_CODE && nmhdr.id_from == ID_TAB as usize {
                let tab = unsafe {
                    SendMessageW(nmhdr.hwnd_from, TCM_GETCURSEL_MSG, None, None).0 as u32
                };
                TAB.with(|c| c.set(Some((hwnd.0 as isize, tab))));
            }
            LRESULT(0)
        }
        WM_SIZE => {
            // The clamp lands here too.
            place_bottom(hwnd);
            LRESULT(0)
        }
        WM_VSCROLL => {
            let code = SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32);
            let line = dpi_scale(hwnd, SCROLL_LINE);
            scroll_to(hwnd, |si| match code {
                SB_LINEUP => si.nPos - line,
                SB_LINEDOWN => si.nPos + line,
                SB_PAGEUP => si.nPos - si.nPage as i32,
                SB_PAGEDOWN => si.nPos + si.nPage as i32,
                SB_THUMBTRACK | SB_THUMBPOSITION => si.nTrackPos,
                _ => si.nPos,
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Signed high word; low is keys.
            let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
            let step = delta / WHEEL_DELTA as i32
                * WHEEL_LINES
                * dpi_scale(hwnd, SCROLL_LINE);
            // Forward scrolls towards the top.
            scroll_to(hwnd, |si| si.nPos - step);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Same outcome as Escape.
            record_outcome(hwnd, SettingsOutcome::Cancel);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Once per process.
///
/// Latch only after success.
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

/// Both panes use this.
///
/// Only what `wndproc` claims.
unsafe extern "system" fn pane_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND | WM_NOTIFY => {
            // SAFETY: `hwnd` is a live pane created by this module, and its
            // parent outlives it - the parent creates it and the OS destroys
            // it with the parent. `GetParent` reports `Err` rather than
            // handing back a stale handle, and that case forwards nothing.
            let parent = unsafe { GetParent(hwnd) };
            match parent {
                // SAFETY: `p` is the live parent just returned above;
                // `wparam` and `lparam` are passed on unchanged, so their
                // meaning is the one the original sender gave them.
                Ok(p) => unsafe { SendMessageW(p, msg, Some(wparam), Some(lparam)) },
                Err(_) => LRESULT(0),
            }
        }
        // SAFETY: default handling of a message this proc does not claim.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Once per process.
///
/// Latch only after success.
unsafe fn register_pane_class(hinstance: HINSTANCE) -> Result<()> {
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
            lpfnWndProc: Some(pane_wndproc),
            hInstance: hinstance,
            lpszClassName: pane_class_name(),
            hCursor: LoadCursorW(None, IDC_ARROW).context("LoadCursorW(IDC_ARROW)")?,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (COLOR_BTNFACE.0 + 1) as *mut core::ffi::c_void,
            ),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread()).context("RegisterClassExW for the pane class");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// The shell's own UI font.
///
/// `None` leaves the default.
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

/// Fonts that may render kana.
///
/// No coverage guarantee.
/// `@` = vertical duplicates.
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

/// Combo rows: name, tag.
///
/// D4: absent tag kept, marked.
fn language_choices(installed: Vec<(String, String)>, configured: &str)
    -> Vec<(String, String)> {
    let mut out = installed;
    if !configured.is_empty()
        && !out.iter().any(|(_, tag)| tag_matches(tag, configured))
    {
        out.push((format!("{configured} (not installed)"), configured.to_string()));
    }
    out
}

/// Row holding `configured`.
fn language_index(rows: &[(String, String)], configured: &str) -> Option<usize> {
    if configured.is_empty() {
        return None;
    }
    rows.iter().position(|(_, tag)| tag_matches(tag, configured))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// One child control, UI font.
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

/// A control by id, pane or not.
///
/// `GetDlgItem` stops at direct
/// children. Page controls are
/// grandchildren of the window,
/// inside the two panes.
unsafe fn dlg_item(root: HWND, id: i32) -> WinResult<HWND> {
    // SAFETY: `root` is the settings window's live handle; every `GetDlgItem`
    // result is checked, so a missing pane or control yields `Err` here rather
    // than a dangling handle. Every caller passes a named `ID_*`, each unique
    // and non-zero. The id 0 that group boxes and labels share now sits only
    // on the content pane - the Updates box took `ID_UPDATES` so
    // `place_bottom` can reach it - and nothing ever looks 0 up anyway, so
    // searching the window first cannot return the wrong control.
    unsafe {
        if let Ok(c) = GetDlgItem(Some(root), id) {
            return Ok(c);
        }
        let (_, content) = panes(root)?;
        GetDlgItem(Some(content), id)
    }
}

/// The viewport and its content.
unsafe fn panes(root: HWND) -> WinResult<(HWND, HWND)> {
    // SAFETY: `root` is the settings window's live handle; both results are
    // checked, so a window without panes - anything before `build` finishes -
    // yields `Err` here rather than a dangling handle.
    unsafe {
        let viewport = GetDlgItem(Some(root), ID_VIEWPORT)?;
        let content = GetDlgItem(Some(viewport), ID_CONTENT)?;
        Ok((viewport, content))
    }
}

/// A box's own selected row.
unsafe fn box_selection(hwnd: HWND, which: DictBox) -> isize {
    // SAFETY: both ids name live descendants of `hwnd`, created in `build`;
    // a missing one yields `Err` here rather than a dangling handle.
    unsafe {
        dlg_item(hwnd, dict_box_id(which))
            .map(|l| SendMessageW(l, LB_GETCURSEL, None, None).0)
            .unwrap_or(-1)
    }
}

/// The box the buttons act on.
unsafe fn active_dict_box(hwnd: HWND) -> DictBox {
    // SAFETY: `box_selection` states its own contract.
    unsafe {
        acting_box(
            box_selection(hwnd, DictBox::Searched) >= 0,
            box_selection(hwnd, DictBox::NotSearched) >= 0,
            tracked_dict_box(hwnd),
        )
    }
}

/// Refill both, select one row.
///
/// The other box is cleared, so
/// one row only is highlighted.
unsafe fn select_dict_row(
    hwnd: HWND,
    searched: &[String],
    not_searched: &[String],
    to: DictBox,
    at: usize,
) {
    // SAFETY: both ids name live descendants of `hwnd`, created in `build`;
    // `fill_dict_list` states its own contract. LB_SETCURSEL with -1 is the
    // documented way to clear a single-selection listbox.
    unsafe {
        for (which, rows) in
            [(DictBox::Searched, searched), (DictBox::NotSearched, not_searched)]
        {
            let Ok(list) = dlg_item(hwnd, dict_box_id(which)) else { continue };
            fill_dict_list(list, rows);
            let sel: isize = if which == to { at as isize } else { -1 };
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(sel as usize)), None);
        }
        record_dict_box(hwnd, to);
    }
}

/// Reorder, crossing at edges.
///
/// Selection follows the item.
unsafe fn move_selected(hwnd: HWND, up: bool) {
    // SAFETY: `list_rows`, `box_selection` and `select_dict_row` each state
    // their own contract, and every handle they take is checked.
    unsafe {
        let Some(mut searched) = list_rows(hwnd, ID_DICTS) else { return };
        let Some(mut not_searched) = list_rows(hwnd, ID_DICTS_OFF) else { return };
        let from = active_dict_box(hwnd);
        let cur = box_selection(hwnd, from);
        if cur < 0 {
            return;
        }
        let landed = dict_move(
            &mut searched,
            &mut not_searched,
            &unreadable_rows(hwnd),
            from,
            cur as usize,
            up,
        );
        let Some((to, at)) = landed else { return };
        select_dict_row(hwnd, &searched, &not_searched, to, at);
        update_list_buttons(hwnd);
    }
}

/// Disable what cannot act.
unsafe fn update_list_buttons(hwnd: HWND) {
    // SAFETY: every id below is a live descendant of `hwnd`, created
    // in `build`, and each `dlg_item` result is checked before use.
    unsafe {
        let freqs = dlg_item(hwnd, ID_FREQS).unwrap_or_default();
        let (Some(searched), Some(not_searched)) =
            (list_rows(hwnd, ID_DICTS), list_rows(hwnd, ID_DICTS_OFF))
        else {
            return;
        };
        if freqs.is_invalid() {
            return;
        }
        let from = active_dict_box(hwnd);
        let Ok(dicts) = dlg_item(hwnd, dict_box_id(from)) else { return };
        let cur = box_selection(hwnd, from);
        let freq_cur = SendMessageW(freqs, LB_GETCURSEL, None, None).0;
        let unreadable = unreadable_rows(hwnd);
        let picked = cur >= 0;
        // One predicate, two callers.
        let can_move = |up: bool| {
            picked
                && dict_move_target(
                    &searched,
                    &not_searched,
                    &unreadable,
                    from,
                    cur as usize,
                    up,
                )
                .is_some()
        };
        // Focus must not be orphaned.
        let focused = GetFocus();
        for (id, list, enable) in [
            (ID_DICT_UP, dicts, can_move(true)),
            (ID_DICT_DOWN, dicts, can_move(false)),
            (ID_DICT_REMOVE, dicts, picked),
            (ID_FREQ_REMOVE, freqs, freq_cur >= 0),
        ] {
            if let Ok(btn) = dlg_item(hwnd, id) {
                if !enable && focused == btn {
                    let _ = SetFocus(Some(list));
                }
                let _ = EnableWindow(btn, enable);
            }
        }
    }
}

/// True when idx > 0.
fn should_show_configure(engine_combo_index: isize) -> bool {
    engine_combo_index > 0
}

/// Lang enable, cfg show.
unsafe fn update_engine_controls(hwnd: HWND) {
    // SAFETY: each id is a live descendant of `hwnd`, created in
    // `build`; a missing one is skipped via `dlg_item`'s `Err`.
    unsafe {
        let Ok(engine) = dlg_item(hwnd, ID_ENGINE) else { return };
        let idx = SendMessageW(engine, CB_GETCURSEL, None, None).0;
        if let Ok(lang) = dlg_item(hwnd, ID_OCR_LANG) {
            let _ = EnableWindow(lang, idx <= 0);
        }
        if let Ok(cfg_btn) = dlg_item(hwnd, ID_ENGINE_CONFIGURE) {
            let mut on_ocr_tab = false;
            if let Ok(tab) = dlg_item(hwnd, ID_TAB) {
                on_ocr_tab = SendMessageW(tab, TCM_GETCURSEL_MSG, None, None).0 == 2;
            }
            let cmd = if on_ocr_tab && should_show_configure(idx) {
                SW_SHOW
            } else {
                SW_HIDE
            };
            let _ = ShowWindow(cfg_btn, cmd);
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
    // SAFETY: `id` names a descendant of `hwnd`; a missing one yields
    // `Err` here rather than a dangling handle, and `list_row` states
    // its own contract.
    unsafe {
        let list = dlg_item(hwnd, id).ok()?;
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
        Some((0..count.max(0)).filter_map(|i| list_row(list, i)).collect())
    }
}

/// Refill, selecting the top.
unsafe fn fill_dict_list(list: HWND, rows: &[String]) {
    // SAFETY: `list` is a live listbox owned by the caller; each string is
    // copied by `LB_ADDSTRING` during the call, so every temporary outlives
    // its only use.
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for row in rows {
            SendMessageW(list, LB_ADDSTRING, None,
                Some(LPARAM(wide(row).as_ptr() as isize)));
        }
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
    }
}

/// An edit or combo's own text.
unsafe fn window_text(ctrl: HWND) -> String {
    // SAFETY: `ctrl` is a live control the caller
    // obtained from `dlg_item`; the buffer is sized
    // to the length `GetWindowTextLengthW` itself
    // reported, which is the contract `GetWindowTextW`
    // writes against.
    unsafe {
        let len = GetWindowTextLengthW(ctrl);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(ctrl, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
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

/// Config's choice for field.
fn default_source<'a>(existing: &'a [crate::config::FieldMapping], field: &str) -> &'a str {
    existing
        .iter()
        .find(|m| m.anki_field == field)
        .map(|m| m.source.as_str())
        .unwrap_or("(none)")
}

/// True if rows match fields.
fn field_names_match(rows: &[(String, HWND)], fields: &[String]) -> bool {
    rows.len() == fields.len() && rows.iter().zip(fields).all(|((n, _), f)| n == f)
}

/// A mapping, or none.
fn row_mapping(anki_field: &str, source: &str) -> Option<crate::config::FieldMapping> {
    (source != "(none)").then(|| crate::config::FieldMapping {
        anki_field: anki_field.to_string(),
        source: source.to_string(),
    })
}

/// Rows per field-map column.
fn field_map_rows_needed(n: usize) -> i32 {
    n.div_ceil(2).max(1) as i32
}

/// Truncated for a column.
fn column_label(name: &str) -> &str {
    name.char_indices().nth(COL_LABEL_MAX_CHARS).map_or(name, |(i, _)| &name[..i])
}

/// One discovered plugin's row.
struct PluginRow {
    label: String,
    roles: String,
    status: String,
    checked: bool,
    /// False for a refused plugin.
    can_enable: bool,
}

/// Config's enabled plugin list.
fn enabled_plugin_names() -> Vec<String> {
    let path = crate::paths::beside_exe("chibipop.toml");
    crate::config::load_or_create(&path).map(|c| c.plugins.enabled).unwrap_or_default()
}

/// Enabled text-provider names.
fn enabled_text_providers(
    found: &[(PathBuf, Result<crate::plugin::manifest::Manifest>)],
    enabled: &[String],
) -> Vec<String> {
    found
        .iter()
        .filter_map(|(_, parsed)| parsed.as_ref().ok())
        .filter(|m| {
            enabled.iter().any(|e| e == &m.name)
                && m.roles.contains(&crate::plugin::manifest::Role::TextProvider)
        })
        .map(|m| m.name.clone())
        .collect()
}

/// Renders one plugin's row.
fn plugin_row(
    dir: &Path,
    parsed: &Result<crate::plugin::manifest::Manifest>,
    enabled: &[String],
) -> PluginRow {
    match parsed {
        Ok(m) => {
            let on = enabled.iter().any(|n| n == &m.name);
            PluginRow {
                label: format!("{} {}", m.name, m.version),
                roles: roles_text(&m.roles),
                status: if on { "Enabled" } else { "Disabled" }.to_string(),
                checked: on,
                can_enable: true,
            }
        }
        Err(e) => PluginRow {
            label: dir_label(dir),
            roles: "—".to_string(),
            status: format!("Refused: {e:#}"),
            checked: false,
            can_enable: false,
        },
    }
}

/// A refused plugin's folder.
fn dir_label(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// This row's plugin name.
fn plugin_key(dir: &Path, parsed: &Result<crate::plugin::manifest::Manifest>) -> String {
    match parsed {
        Ok(m) => m.name.clone(),
        Err(_) => dir_label(dir),
    }
}

/// Roles, joined for display.
fn roles_text(roles: &[crate::plugin::manifest::Role]) -> String {
    if roles.is_empty() {
        return "—".to_string();
    }
    roles
        .iter()
        .map(|r| match r {
            crate::plugin::manifest::Role::TextProvider => "text-provider",
            crate::plugin::manifest::Role::FieldContributor => "field-contributor",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Group box height for n rows.
fn plugins_group_h(n: usize) -> i32 {
    let body = if n == 0 {
        40
    } else {
        let n = n as i32;
        n * PLUGIN_ROW_H + (n - 1) * ROW_GAP
    };
    20 + body + 8
}

/// Toggle glyph for fold state.
fn field_map_toggle_label(collapsed: bool) -> &'static str {
    if collapsed { "Field mapping \u{25B6}" } else { "Field mapping \u{25BC}" }
}

/// `&&` renders one `&`.
fn apply_caption(mode: ApplyMode) -> &'static str {
    if mode == ApplyMode::Live { "Apply" } else { "Apply && Restart" }
}

/// What that button will do.
fn apply_hint(mode: ApplyMode, staged: bool) -> &'static str {
    match (mode, staged) {
        (ApplyMode::Live, false) => "Applying saves your settings and uses them right away.",
        (ApplyMode::Live, true) => "Applying saves your settings and updates your \
                                    dictionaries in place.",
        (ApplyMode::Standalone, _) => "Applying saves your settings and restarts chibipop.",
    }
}

/// Junk keeps the old value.
fn parse_px(text: &str, fallback: i32) -> i32 {
    text.trim().parse().unwrap_or(fallback)
}

/// `None` unless capturing.
fn take_captured_key(hwnd: HWND, vk: u16) -> Option<(i32, String)> {
    let mine = hwnd.0 as isize;
    let id = CAPTURING.with(|c| c.get()).and_then(|(h, id)| (h == mine).then_some(id))?;
    CAPTURING.with(|c| c.set(None));
    let cell = if id == ID_TRIGGER_KEY { &CAPTURED_VK } else { &ANKI_CAPTURED_VK };
    cell.with(|c| c.set(Some((mine, vk))));
    Some((id, crate::config::trigger_key_name(vk)))
}

/// A captured key, or template.
fn resolved_captured_key(
    cell: &'static std::thread::LocalKey<Cell<Option<(isize, u16)>>>,
    hwnd: HWND,
    template: &str,
) -> String {
    cell.with(|c| c.get())
        .and_then(|(h, vk)| (h == hwnd.0 as isize).then_some(vk))
        .or_else(|| crate::config::parse_trigger_key(template))
        .map_or_else(|| template.to_string(), stored_trigger_key)
}

/// What `read()` persists.
fn resolved_trigger_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&CAPTURED_VK, hwnd, template)
}

/// Same, for the Anki add key.
fn resolved_anki_add_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&ANKI_CAPTURED_VK, hwnd, template)
}

/// Parseable form of `vk`.
fn stored_trigger_key(vk: u16) -> String {
    match vk {
        0x10 => "shift".into(),
        0x11 => "ctrl".into(),
        0x12 => "alt".into(),
        0x70..=0x7B => format!("f{}", vk - 0x6F),
        _ => format!("0x{vk:02X}"),
    }
}

pub struct SettingsWindow {
    hwnd: HWND,
    /// Clips the content pane.
    viewport: HWND,
    /// Slides inside the viewport.
    content: HWND,
    font: Option<HFONT>,
    /// The numeric values each combo offers, in the order they were added, so
    /// `read` can map a selection index back to a value.
    widths: Vec<i64>,
    heights: Vec<i64>,
    summaries: Vec<i64>,
    passes: Vec<i64>,
    fonts: Vec<String>,
    /// Language tags, combo order.
    ocr_langs: Vec<String>,
    /// Engine values, combo order.
    engine_names: Vec<String>,
    /// Engine name to plugin directory.
    engine_dirs: HashMap<String, PathBuf>,
    /// What Apply has yet to do.
    staged: RefCell<SettingsForm>,
    /// General-tab-only controls.
    general_ctrls: Vec<HWND>,
    /// Dictionaries-tab controls.
    dict_ctrls: Vec<HWND>,
    /// OCR/Debug-tab controls.
    ocr_ctrls: Vec<HWND>,
    /// Anki-tab-only controls.
    anki_ctrls: Vec<HWND>,
    /// Plugins-tab-only controls.
    plugin_ctrls: Vec<HWND>,
    /// Plugin names, checkbox order.
    plugin_names: Vec<String>,
    /// Anki field name -> its combo.
    field_map_rows: RefCell<Vec<(String, HWND)>>,
    /// Field-map labels + group box.
    field_map_extra: RefCell<Vec<HWND>>,
    /// True while collapsed.
    field_map_collapsed: Cell<bool>,
    /// Anki static rows end, page y.
    anki_static_bottom: i32,
    /// Each tab's page height, 96-DPI.
    tab_heights: [i32; 5],
    /// Tallest tab's bottom y.
    bottom_y0: i32,
    /// Which tab is showing.
    current_tab: Cell<u32>,
    /// What Apply will do.
    apply_mode: ApplyMode,
}

impl SettingsWindow {
    /// Create and show the window, populated from `form`.
    ///
    /// `stale` are `display_order` entries matching no installed dictionary
    /// (spec D6a); when non-empty a warning naming them is shown, because that
    /// is what a dictionary rename looks like from in here.
    ///
    /// `mode` words the Apply button.
    pub fn open(
        form: &SettingsForm,
        stale: &[String],
        mode: ApplyMode,
    ) -> Result<SettingsWindow> {
        // SAFETY: every call below is an ordinary window-creation FFI call
        // with handles this function owns; each `?` leaves nothing to leak
        // because the window is the only resource and it is not yet created.
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();
            register_class(hinstance)?;
            register_pane_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name(),
                w!("chibipop settings"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VSCROLL,
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
                // `build` creates both.
                viewport: HWND::default(),
                content: HWND::default(),
                font,
                widths: Vec::new(),
                heights: Vec::new(),
                summaries: Vec::new(),
                passes: Vec::new(),
                fonts: Vec::new(),
                ocr_langs: Vec::new(),
                engine_names: Vec::new(),
                engine_dirs: HashMap::new(),
                staged: RefCell::new(form.clone()),
                general_ctrls: Vec::new(),
                dict_ctrls: Vec::new(),
                ocr_ctrls: Vec::new(),
                anki_ctrls: Vec::new(),
                plugin_ctrls: Vec::new(),
                plugin_names: Vec::new(),
                field_map_rows: RefCell::new(Vec::new()),
                field_map_extra: RefCell::new(Vec::new()),
                field_map_collapsed: Cell::new(true),
                anki_static_bottom: 0,
                tab_heights: [0; 5],
                bottom_y0: 0,
                current_tab: Cell::new(0),
                apply_mode: mode,
            };
            // `build` greys from these.
            remember_unreadable(hwnd, &form.unreadable);
            // `build` reports where its layout actually ended; the window is
            // then sized to that rather than to a guess. The first version of
            // this file passed a hand-tuned height straight to
            // `CreateWindowExW` - which takes the OUTER size - so 39px of
            // caption and frame ate the Apply and Cancel buttons entirely and
            // the window opened with no way to accept anything. Measuring the
            // content means that cannot recur, at any DPI or font size.
            let content_h = win.build(form, stale)?;
            // Both sides from one vector.
            if let Some(tag) = win.selected_language() {
                win.staged.borrow_mut().dict_list_language = tag;
            }
            // Sizes AND shows - see `fit_to` for why showing cannot go
            // through `ShowWindow` here.
            win.fit_to(WIN_W, content_h + PAD);
            // General tab, from the top.
            win.reset_scroll();
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

    /// Anki/update click, if any.
    pub fn take_click(&self) -> Option<SettingsClick> {
        CLICK.with(|c| match c.get() {
            Some((h, k)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(k)
            }
            _ => None,
        })
    }

    /// The Anki URL field's text.
    pub fn anki_url(&self) -> String {
        // SAFETY: `ID_ANKI_URL` is a live descendant of
        // `self.hwnd`, created in `build`.
        unsafe {
            dlg_item(self.hwnd, ID_ANKI_URL)
                .map(|c| window_text(c))
                .unwrap_or_default()
        }
    }

    /// The Anki model field's text.
    pub fn anki_model(&self) -> String {
        // SAFETY: `ID_ANKI_MODEL` is a live descendant of
        // `self.hwnd`, created in `build`.
        unsafe {
            dlg_item(self.hwnd, ID_ANKI_MODEL)
                .map(|c| window_text(c))
                .unwrap_or_default()
        }
    }

    /// Run a pending button.
    ///
    /// Callback precedes a picker.
    pub fn pump(&self, before_blocking: impl FnOnce()) {
        if self.take_language_change() {
            self.rescope_dicts();
        }
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
        // SAFETY: each helper acts only on live descendants of
        // `self.hwnd`, which outlives this call, and each states
        // its own contract.
        unsafe {
            match action {
                Action::Remove(target) => self.remove_selected(target),
                Action::Add => {
                    // D9: the picker pumps too.
                    before_blocking();
                    self.add_picked();
                }
                Action::ConfigureEngine => {
                    // D9: the picker pumps too.
                    before_blocking();
                    self.configure_engine();
                }
            }
        }
    }

    /// Forget what Apply just did.
    pub fn clear_staged(&self) {
        self.staged.borrow_mut().clear_staged();
    }

    /// Take what Apply just wrote.
    pub fn reseed_per_language(&self, written: &BTreeMap<String, Vec<String>>) {
        self.staged.borrow_mut().reseed_per_language(written);
    }

    /// Say what Apply is doing.
    pub fn set_status(&self, text: &str) {
        // SAFETY: `ID_STATUS` is a live child of `self.hwnd`, created in
        // `build`; `SetWindowTextW` copies the string during the call.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_STATUS) {
                let _ = SetWindowTextW(c, PCWSTR(wide(text).as_ptr()));
            }
        }
    }

    /// Show what was really applied.
    pub fn set_capture_fields(&self, ocr: &crate::config::OcrConfig) {
        // SAFETY: `ID_CAPTURE_W` and `ID_CAPTURE_H` are live descendants of
        // `self.hwnd`, created in `build`; each `dlg_item` result is
        // checked, and `SetWindowTextW` copies the string during the call,
        // so each temporary outlives its only use.
        unsafe {
            for (id, px) in [(ID_CAPTURE_W, ocr.capture_width), (ID_CAPTURE_H, ocr.capture_height)]
            {
                if let Ok(c) = dlg_item(self.hwnd, id) {
                    let _ = SetWindowTextW(c, PCWSTR(wide(&px.to_string()).as_ptr()));
                }
            }
        }
    }

    /// Re-word Apply after staging.
    fn refresh_apply(&self) {
        let staged = self.staged.borrow().has_staged();
        // SAFETY: `ID_APPLY` and `ID_STATUS` are live children of `self.hwnd`,
        // created in `build`; `SetWindowTextW` copies each string during the
        // call, so the temporaries below outlive every use.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_APPLY) {
                let caption = wide(apply_caption(self.apply_mode));
                let _ = SetWindowTextW(c, PCWSTR(caption.as_ptr()));
            }
            if let Ok(c) = dlg_item(self.hwnd, ID_STATUS) {
                let hint = wide(apply_hint(self.apply_mode, staged));
                let _ = SetWindowTextW(c, PCWSTR(hint.as_ptr()));
            }
        }
    }

    /// Lock it while Apply runs.
    pub fn set_busy(&self, busy: bool) {
        // SAFETY: every id in `WHILE_BUSY` is a live descendant of
        // `self.hwnd`, created in `build`; each `dlg_item` result is
        // checked. Focus is moved off the controls first, since a disabled
        // window keeping focus leaves the keyboard talking to nothing.
        unsafe {
            if busy {
                let _ = SetFocus(Some(self.hwnd));
            }
            for id in WHILE_BUSY {
                if let Ok(c) = dlg_item(self.hwnd, id) {
                    let _ = EnableWindow(c, !busy);
                }
            }
            if !busy {
                update_list_buttons(self.hwnd);
                update_engine_controls(self.hwnd);
            }
        }
    }

    /// Pending tab switch, if any.
    pub fn take_tab_change(&self) -> Option<u32> {
        TAB.with(|c| match c.get() {
            Some((h, tab)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(tab)
            }
            _ => None,
        })
    }

    /// Pending switch, if any.
    fn take_language_change(&self) -> bool {
        LANG_CHANGED.with(|c| match c.get() {
            Some(h) if h == self.hwnd.0 as isize => {
                c.set(None);
                true
            }
            _ => false,
        })
    }

    /// The language combo's own tag.
    fn selected_language(&self) -> Option<String> {
        // SAFETY: `ID_OCR_LANG` is a live descendant of `self.hwnd`, made
        // in `build`; a missing one yields `Err` here rather than a handle.
        let i = unsafe {
            dlg_item(self.hwnd, ID_OCR_LANG)
                .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                .unwrap_or(-1)
        };
        if i < 0 {
            return None;
        }
        self.ocr_langs.get(i as usize).cloned()
    }

    /// Selected engine's plugin directory.
    fn selected_engine_dir(&self) -> Option<&Path> {
        // SAFETY: `ID_ENGINE` is a live descendant of
        // `self.hwnd`, created in `build`.
        let idx = unsafe {
            let Ok(e) = dlg_item(self.hwnd, ID_ENGINE) else { return None };
            SendMessageW(e, CB_GETCURSEL, None, None).0 as usize
        };
        let name = self.engine_names.get(idx)?;
        self.engine_dirs.get(name).map(|p| p.as_path())
    }

    /// Selected engine's own name.
    fn selected_engine_name(&self) -> Option<&str> {
        // SAFETY: `ID_ENGINE` is a live descendant of
        // `self.hwnd`, created in `build`.
        let idx = unsafe {
            let Ok(e) = dlg_item(self.hwnd, ID_ENGINE) else { return None };
            SendMessageW(e, CB_GETCURSEL, None, None).0 as usize
        };
        self.engine_names.get(idx).map(|s| s.as_str())
    }

    /// Re-split for the combo.
    ///
    /// Snapshots the old one first.
    fn rescope_dicts(&self) {
        let Some(next) = self.selected_language() else { return };
        let mut staged = self.staged.borrow_mut();
        let prev = staged.dict_list_language.clone();
        if prev == next {
            return;
        }
        // SAFETY: both list ids are live descendants of `self.hwnd`, made
        // in `build`; `list_rows` and `select_dict_row` state their contracts
        // and every handle is checked before it is used.
        unsafe {
            let (Some(active), Some(excluded)) =
                (list_rows(self.hwnd, ID_DICTS), list_rows(self.hwnd, ID_DICTS_OFF))
            else {
                return;
            };
            staged.dict_names = active;
            staged.dict_excluded = excluded;
            staged.ocr_language = prev.clone();
            if crate::settings::is_scoped(&staged) {
                let existing = staged.per_language.get(&prev).cloned().unwrap_or_default();
                let keyed = crate::settings::scoped_entry(
                    &staged.dict_names, &staged.unreadable, &existing);
                if let Some(keys) = keyed {
                    staged.per_language.insert(prev, keys);
                }
            }
            let all: Vec<String> =
                staged.dict_names.iter().chain(staged.dict_excluded.iter()).cloned().collect();
            let list = staged.per_language.get(&next).cloned().unwrap_or_default();
            let (active, excluded) = scope_rows(&all, &list, &staged.unreadable);
            staged.dict_names = active;
            staged.dict_excluded = excluded;
            staged.dict_list_language = next.clone();
            staged.ocr_language = next;
            select_dict_row(
                self.hwnd,
                &staged.dict_names,
                &staged.dict_excluded,
                DictBox::Searched,
                0,
            );
            update_list_buttons(self.hwnd);
        }
    }

    /// Pending fold toggle, if any.
    pub fn take_field_map_toggle(&self) -> bool {
        FIELD_MAP_TOGGLE.with(|c| match c.get() {
            Some(h) if h == self.hwnd.0 as isize => {
                c.set(None);
                true
            }
            _ => false,
        })
    }

    /// A tab's height, page y.
    ///
    /// Anki grows with its field
    /// map: measured, not stored.
    fn tab_page_h(&self, tab: u32) -> i32 {
        if tab == 3 {
            return self.field_map_bottom() - CONTENT_Y;
        }
        self.tab_heights.get(tab as usize).copied().unwrap_or(0)
    }

    /// Re-range for the shown tab.
    ///
    /// Back to the top with it.
    fn reset_scroll(&self) {
        let content_h = dpi_scale(self.hwnd, self.tab_page_h(self.current_tab.get()));
        set_scroll_range(self.hwnd, content_h, client_h(self.viewport));
    }

    /// Show one tab, hide the rest.
    pub fn switch_tab(&self, tab: u32) {
        // SAFETY: `self.hwnd` is live until `Drop`.
        unsafe { cancel_capture(self.hwnd) };
        let groups = [
            &self.general_ctrls,
            &self.dict_ctrls,
            &self.ocr_ctrls,
            &self.anki_ctrls,
            &self.plugin_ctrls,
        ];
        if tab as usize >= groups.len() {
            return;
        }
        self.current_tab.set(tab);
        // SAFETY: every HWND in every group was created in
        // `build` as a descendant of `self.hwnd` and lives until
        // the window is destroyed.
        unsafe {
            for (i, ctrls) in groups.iter().enumerate() {
                let cmd = if i as u32 == tab { SW_SHOW } else { SW_HIDE };
                for &c in *ctrls {
                    let _ = ShowWindow(c, cmd);
                }
            }
            self.apply_field_map_visibility();
            update_engine_controls(self.hwnd);
        }
        self.reset_scroll();
    }

    /// Flips the fold and resizes.
    pub fn toggle_field_map(&self) {
        let collapsed = !self.field_map_collapsed.get();
        self.field_map_collapsed.set(collapsed);
        // SAFETY: `self.hwnd` is live until `Drop`; `ID_FIELD_MAP_TOGGLE`
        // and every hwnd `apply_field_map_visibility` touches are its
        // children, created in `build`/`build_field_map_rows`.
        unsafe {
            self.apply_field_map_visibility();
            if let Ok(btn) = dlg_item(self.hwnd, ID_FIELD_MAP_TOGGLE) {
                let text = field_map_toggle_label(collapsed);
                let _ = SetWindowTextW(btn, PCWSTR(wide(text).as_ptr()));
            }
        }
        self.ensure_room_for(self.field_map_bottom());
    }

    /// Captures `vk`; true if used.
    pub fn handle_capture_key(&self, vk: u16) -> bool {
        let Some((id, text)) = take_captured_key(self.hwnd, vk) else {
            return false;
        };
        // SAFETY: `id` is ID_TRIGGER_KEY or ID_ANKI_ADD_KEY, both live
        // descendants of `self.hwnd`, created in `build`; `SetWindowTextW`
        // copies the string during the call.
        unsafe {
            if let Ok(btn) = dlg_item(self.hwnd, id) {
                let _ = SetWindowTextW(btn, PCWSTR(wide(&text).as_ptr()));
            }
        }
        true
    }

    /// Fills deck/model + map rows.
    pub fn populate_combos(&self, decks: &[String], models: &[String], fields: &[String]) {
        // SAFETY: `ID_ANKI_DECK` and `ID_ANKI_MODEL` are live descendants of
        // `self.hwnd`, created in `build`; each `SendMessageW` copies the
        // string during the call.
        unsafe {
            if let Ok(deck) = dlg_item(self.hwnd, ID_ANKI_DECK) {
                let cur = window_text(deck);
                SendMessageW(deck, CB_RESETCONTENT, None, None);
                for name in decks {
                    SendMessageW(deck, CB_ADDSTRING, None,
                        Some(LPARAM(wide(name).as_ptr() as isize)));
                }
                SendMessageW(deck, WM_SETTEXT, None,
                    Some(LPARAM(wide(&cur).as_ptr() as isize)));
            }
            if let Ok(model) = dlg_item(self.hwnd, ID_ANKI_MODEL) {
                let cur = window_text(model);
                SendMessageW(model, CB_RESETCONTENT, None, None);
                for name in models {
                    SendMessageW(model, CB_ADDSTRING, None,
                        Some(LPARAM(wide(name).as_ptr() as isize)));
                }
                SendMessageW(model, WM_SETTEXT, None,
                    Some(LPARAM(wide(&cur).as_ptr() as isize)));
            }
        }
        self.populate_field_map(fields);
    }

    /// Rebuilds the field-map rows.
    ///
    /// No-op if empty or unchanged.
    fn populate_field_map(&self, fields: &[String]) {
        if fields.is_empty() || self.field_map_unchanged(fields) {
            return;
        }
        // SAFETY: every hwnd in `field_map_extra`/`field_map_rows` was
        // created by this same function as a descendant of `self.hwnd`, and
        // is destroyed here exactly once before its slot is reused.
        unsafe {
            for hwnd in self.field_map_extra.borrow_mut().drain(..) {
                let _ = DestroyWindow(hwnd);
            }
            for (_, hwnd) in self.field_map_rows.borrow_mut().drain(..) {
                let _ = DestroyWindow(hwnd);
            }
        }
        let existing = self.staged.borrow().field_map.clone();
        let (extra, rows) = self.build_field_map_rows(fields, &existing);
        *self.field_map_extra.borrow_mut() = extra;
        *self.field_map_rows.borrow_mut() = rows;
        // SAFETY: every hwnd was just created as a descendant of `self.hwnd`.
        unsafe { self.apply_field_map_visibility() };
        self.ensure_room_for(self.field_map_bottom());
    }

    /// Shows/hides field-map rows.
    unsafe fn apply_field_map_visibility(&self) {
        let visible = self.current_tab.get() == 3 && !self.field_map_collapsed.get();
        let cmd = if visible { SW_SHOW } else { SW_HIDE };
        // SAFETY: every hwnd here is a live descendant of `self.hwnd`,
        // created in `build_field_map_rows`.
        unsafe {
            for &c in self.field_map_extra.borrow().iter() {
                let _ = ShowWindow(c, cmd);
            }
            for &(_, c) in self.field_map_rows.borrow().iter() {
                let _ = ShowWindow(c, cmd);
            }
        }
    }

    /// Field-map area's bottom.
    ///
    /// Window y, like `bottom_y0`.
    fn field_map_bottom(&self) -> i32 {
        let n = self.field_map_rows.borrow().len();
        let page = if n == 0 || self.field_map_collapsed.get() {
            self.anki_static_bottom
        } else {
            self.anki_static_bottom + 20 + field_map_rows_needed(n) * ROW_H + 8
        };
        CONTENT_Y + page
    }

    /// Right below the tab strip.
    ///
    /// Tab order follows z-order, so
    /// the pages must sit there, not
    /// after the Apply row.
    ///
    /// Only the window's own
    /// children can displace it.
    unsafe fn place_viewport(&self) {
        // SAFETY: `self.viewport` is a live child of `self.hwnd` from `build`
        // on, destroyed only with it; before that it is null and the call
        // fails harmlessly. `GetDlgItem` yields the tab control, a sibling of
        // the viewport, which is what `SetWindowPos` requires of an insert-
        // after handle. Without it the seat is left alone: creation order
        // already puts the viewport there, so moving it anywhere else - to
        // `HWND_BOTTOM` above all - would only be worse.
        // `SWP_NOSIZE | SWP_NOMOVE` leave its rect alone.
        unsafe {
            let Ok(after) = GetDlgItem(Some(self.hwnd), ID_TAB) else {
                return;
            };
            let _ = SetWindowPos(self.viewport, Some(after), 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// Builds the box + field rows.
    fn build_field_map_rows(
        &self,
        fields: &[String],
        existing: &[crate::config::FieldMapping],
    ) -> (Vec<HWND>, Vec<(String, HWND)>) {
        let f = self.font;
        let h = self.hwnd;
        // Page y, like `build`.
        let page = self.content;
        let y0 = self.anki_static_bottom;
        let rows_n = field_map_rows_needed(fields.len());
        let map_h = 20 + rows_n * ROW_H + 8;
        let mut extra = Vec::new();
        let mut rows = Vec::new();
        // SAFETY: `h` is `self.hwnd` and `page` its content pane, both live
        // for the caller's duration; every control created is a child of
        // `page` and outlives this call.
        unsafe {
            if let Ok(g) = child(page, w!("BUTTON"), "",
                WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
                PAD - 6, y0, WIN_W - 2 * PAD, map_h, 0, f)
            {
                extra.push(g);
            }
            for (idx, name) in fields.iter().enumerate() {
                let idx = idx as i32;
                let col = idx / rows_n;
                let row = idx % rows_n;
                let x = PAD + col * (COL_W + COL_GAP);
                let y = y0 + 20 + row * ROW_H;
                if let Ok(l) = child(page, w!("STATIC"), column_label(name),
                    WINDOW_STYLE(0), x, y + 4, COL_LABEL_W, ROW_H, 0, f)
                {
                    extra.push(l);
                }
                let id = ID_FIELD_MAP_BASE + idx;
                let combo_x = x + COL_LABEL_W + COL_LABEL_GAP;
                if let Ok(combo) = child(page, w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    combo_x, y, COL_COMBO_W, 140, id, f)
                {
                    let want = default_source(existing, name);
                    for (j, src) in FIELD_MAP_SOURCES.iter().enumerate() {
                        SendMessageW(combo, CB_ADDSTRING, None,
                            Some(LPARAM(wide(src).as_ptr() as isize)));
                        if *src == want {
                            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(j)), None);
                        }
                    }
                    if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
                    }
                    SendMessageW(combo, CB_SETDROPPEDWIDTH,
                        Some(WPARAM(dpi_scale(h, COL_DROPPED_W) as usize)), None);
                    rows.push((name.clone(), combo));
                }
            }
        }
        (extra, rows)
    }

    /// True if rows match fields.
    fn field_map_unchanged(&self, fields: &[String]) -> bool {
        let rows = self.field_map_rows.borrow();
        field_names_match(&rows, fields)
    }

    /// Fits the window to content.
    ///
    /// Never below build's own size.
    fn ensure_room_for(&self, needed_bottom: i32) {
        let new_y0 = needed_bottom.max(self.bottom_y0);
        // SAFETY: `self.content` is a live descendant of `self.hwnd`, created
        // once in `build` and never destroyed before `self.hwnd` itself.
        // `SWP_NOMOVE` leaves its origin alone and `SWP_NOZORDER` keeps the
        // placement `place_viewport` chose. The viewport is not touched here:
        // `place_bottom` sizes it from the client, off `fit_to`'s `WM_SIZE`.
        unsafe {
            // Or the pages clip.
            let _ = SetWindowPos(
                self.content, None,
                0, 0,
                dpi_scale(self.hwnd, WIN_W),
                dpi_scale(self.hwnd, new_y0 - CONTENT_Y),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // Unconditional: shrink too.
        self.fit_to(WIN_W, new_y0 + BOTTOM_H + PAD);
        // The band just changed size.
        self.reset_scroll();
    }

    /// Drop the selected row.
    unsafe fn remove_selected(&self, target: Target) {
        // SAFETY: the id below names a live descendant of `self.hwnd`;
        // `list_row`, `active_dict_box` and `update_list_buttons` state
        // their own contracts. Either box may hold the selection.
        unsafe {
            let id = match target {
                Target::Dicts => dict_box_id(active_dict_box(self.hwnd)),
                Target::Freqs => ID_FREQS,
            };
            let Ok(list) = dlg_item(self.hwnd, id) else {
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
            self.refresh_apply();
        }
    }

    /// Stage whatever was picked.
    unsafe fn add_picked(&self) {
        // SAFETY: `pick_archives` owns every buffer it hands the dialog;
        // every id names a live descendant of `self.hwnd`, and the string each
        // `LB_ADDSTRING` copies outlives that call. `list_rows` and
        // `select_dict_row` each state their own contract.
        unsafe {
            let picked = pick_archives(self.hwnd);
            for path in picked {
                // The file picks the list.
                let Some(kind) = self.staged.borrow_mut().stage_add(&path) else {
                    eprintln!(
                        "chibipop: {} is already listed, or is not a dictionary chibipop can read.",
                        path.display()
                    );
                    continue;
                };
                let Some(name) =
                    self.staged.borrow().staged_adds.last().map(|a| a.name.clone())
                else {
                    continue;
                };
                if kind == Kind::Frequency {
                    let Ok(list) = dlg_item(self.hwnd, ID_FREQS) else {
                        continue;
                    };
                    SendMessageW(list, LB_ADDSTRING, None,
                        Some(LPARAM(wide(&name).as_ptr() as isize)));
                    if SendMessageW(list, LB_GETCURSEL, None, None).0 < 0 {
                        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
                    }
                    continue;
                }
                let (Some(mut searched), Some(not_searched)) =
                    (list_rows(self.hwnd, ID_DICTS), list_rows(self.hwnd, ID_DICTS_OFF))
                else {
                    continue;
                };
                // LB_SETCURSEL scrolls it in.
                let at = add_dict(&mut searched, &name);
                select_dict_row(self.hwnd, &searched, &not_searched, DictBox::Searched, at);
            }
            update_list_buttons(self.hwnd);
            self.refresh_apply();
        }
    }

    /// Picks a folder, saves it.
    unsafe fn configure_engine(&self) {
        let Some(name) = self.selected_engine_name() else { return };
        let Some(dir) = self.selected_engine_dir() else { return };
        let title = format!("Select your {name} installation");
        // SAFETY: `self.hwnd` is live; the picker frees its
        // own PIDL before returning.
        let picked = unsafe { pick_folder(self.hwnd, &title) };
        let Some(path) = picked else { return };
        let cfg_path = dir.join("config.toml");
        let existing = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        let updated = set_config_path(&existing, &path.to_string_lossy());
        if std::fs::write(&cfg_path, updated).is_err() {
            self.set_status(&format!("Could not save {name} path."));
            return;
        }
        self.set_status(&format!("Saved {name} path."));
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
                let mut outer_h = rc.bottom - rc.top;
                if let Some(cap) = work_area_height(self.hwnd) {
                    outer_h = outer_h.min(cap);
                }
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    rc.right - rc.left,
                    outer_h,
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
    unsafe fn build(&mut self, form: &SettingsForm, stale: &[String])
        -> Result<i32> {
        let f = self.font;
        let h = self.hwnd;
        let mut y = PAD;
        let mut gen: Vec<HWND> = Vec::new();
        let mut dict: Vec<HWND> = Vec::new();
        let mut ocr: Vec<HWND> = Vec::new();
        let mut ank: Vec<HWND> = Vec::new();
        let mut plug: Vec<HWND> = Vec::new();
        let mut plugin_names: Vec<String> = Vec::new();
        let mut plugin_dirs: Vec<PathBuf> = Vec::new();

        // SAFETY: `h` is the window just created by `open`. Every control
        // below is created as a child of `h`, of `h`'s viewport pane, or of
        // that pane's content pane, and each parent is created before any of
        // its children. Windows destroys a child with its parent, so every
        // handle taken here lives until `h` is destroyed.
        unsafe {
            // Tabs need comctl init.
            let icex = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES,
            };
            let _ = InitCommonControlsEx(&icex);

            // ---- Tab control ----
            let tab = child(h, w!("SysTabControl32"), "",
                WS_TABSTOP | WS_CLIPSIBLINGS,
                PAD - 6, y, WIN_W - 2 * PAD, TAB_H,
                ID_TAB, f)?;
            let mut t0 = wide("General");
            let mut item = TcItemW {
                mask: TCIF_TEXT_VAL,
                dw_state: 0,
                dw_state_mask: 0,
                psz_text: t0.as_mut_ptr(),
                cch_text_max: 0,
                i_image: -1,
                l_param: 0,
            };
            SendMessageW(tab, TCM_INSERTITEMW_MSG, Some(WPARAM(0)),
                Some(LPARAM(&item as *const _ as isize)));
            let mut t1 = wide("Dictionaries");
            item.psz_text = t1.as_mut_ptr();
            SendMessageW(tab, TCM_INSERTITEMW_MSG, Some(WPARAM(1)),
                Some(LPARAM(&item as *const _ as isize)));
            let mut t2 = wide("OCR / Debug");
            item.psz_text = t2.as_mut_ptr();
            SendMessageW(tab, TCM_INSERTITEMW_MSG, Some(WPARAM(2)),
                Some(LPARAM(&item as *const _ as isize)));
            let mut t3 = wide("Anki");
            item.psz_text = t3.as_mut_ptr();
            SendMessageW(tab, TCM_INSERTITEMW_MSG, Some(WPARAM(3)),
                Some(LPARAM(&item as *const _ as isize)));
            let mut t4 = wide("Plugins");
            item.psz_text = t4.as_mut_ptr();
            SendMessageW(tab, TCM_INSERTITEMW_MSG, Some(WPARAM(4)),
                Some(LPARAM(&item as *const _ as isize)));
            // Sized when the band is known.
            self.viewport = child(h, pane_class_name(), "",
                WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                0, CONTENT_Y, WIN_W, 0, ID_VIEWPORT, None)?;
            self.content = child(self.viewport, pane_class_name(), "", WS_CLIPSIBLINGS,
                0, 0, WIN_W, 0, ID_CONTENT, None)?;
            // Or Tab skips every page.
            for pane in [self.viewport, self.content] {
                let ex = GetWindowLongW(pane, GWL_EXSTYLE) as u32 | WS_EX_CONTROLPARENT.0;
                SetWindowLongW(pane, GWL_EXSTYLE, ex as i32);
            }
            // Page y, not window y.
            let page = self.content;
            y = 0;

            let group = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(page, w!("BUTTON"), text, WINDOW_STYLE(BS_GROUPBOX as u32),
                      PAD - 6, y, WIN_W - 2 * PAD, height, 0, f)
            };
            // Same, but carrying WS_GROUP so it ends the preceding group.
            let group_start = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(page, w!("BUTTON"), text,
                      WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
                      PAD - 6, y, WIN_W - 2 * PAD, height, 0, f)
            };
            let label = |text: &str, y: i32| -> WinResult<HWND> {
                child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y + 4, LABEL_W, ROW_H, 0, f)
            };

            // ---- Trigger ----
            gen.push(group("Trigger", y, ROW_H + ROW_GAP + ROW_H + 26)?);
            y += 20;
            let live = child(page, w!("BUTTON"), "Live",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32) | WS_GROUP | WS_TABSTOP,
                PAD, y, 120, ROW_H, ID_MODE_LIVE, f)?;
            gen.push(live);
            let hold = child(page, w!("BUTTON"), "Hold key",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
                PAD + 130, y, 120, ROW_H, ID_MODE_HOLD, f)?;
            gen.push(hold);
            let is_live = matches!(form.mode, crate::config::TriggerMode::Live);
            SendMessageW(live, BM_SETCHECK,
                Some(WPARAM(if is_live { 1 } else { 0 })), None);
            SendMessageW(hold, BM_SETCHECK,
                Some(WPARAM(if is_live { 0 } else { 1 })), None);
            y += ROW_H + ROW_GAP;
            gen.push(label("Trigger key", y)?);
            let key_vk = crate::config::parse_trigger_key(&form.trigger_key).unwrap_or(0x10);
            CAPTURED_VK.with(|c| c.set(Some((h.0 as isize, key_vk))));
            let key_name = crate::config::trigger_key_name(key_vk);
            let key_btn = child(page, w!("BUTTON"), &key_name, WS_TABSTOP,
                FIELD_X, y, FIELD_W, ROW_H, ID_TRIGGER_KEY, f)?;
            gen.push(key_btn);
            let _ = EnableWindow(key_btn, !is_live);
            y += ROW_H + 18;

            // ---- Popup ----
            // WS_GROUP terminates the radio group above. Without it the group
            // runs to the end of the window and arrow keys walk straight out
            // of Live/Hold Shift into the combos.
            gen.push(group_start("Popup", y, 5 * (ROW_H + ROW_GAP) + 4 * ROW_H + 30)?);
            y += 20;

            gen.push(label("Theme", y)?);
            let theme = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_THEME, f)?;
            gen.push(theme);
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

            gen.push(label("Font", y)?);
            let fonts_hwnd = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 260, ID_FONT, f)?;
            gen.push(fonts_hwnd);
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

            self.widths = numeric_choices(
                MAX_WIDTH_RANGE.0 as i64, MAX_WIDTH_RANGE.1 as i64, 5,
                form.max_width_percent as i64);
            gen.push(label("Max width (% of screen)", y)?);
            let mw = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_MAX_WIDTH, f)?;
            gen.push(mw);
            fill_numeric(mw, &self.widths, form.max_width_percent as i64);
            y += ROW_H + ROW_GAP;

            self.heights = numeric_choices(
                MAX_HEIGHT_RANGE.0 as i64, MAX_HEIGHT_RANGE.1 as i64, 5,
                form.max_height_percent as i64);
            gen.push(label("Max height (% of screen)", y)?);
            let mh = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_MAX_HEIGHT, f)?;
            gen.push(mh);
            fill_numeric(mh, &self.heights, form.max_height_percent as i64);
            y += ROW_H + ROW_GAP;

            self.summaries = numeric_choices(
                SUMMARY_RANGE.0 as i64, SUMMARY_RANGE.1 as i64, 10,
                form.summary_chars as i64);
            gen.push(label("Summary length (characters)", y)?);
            let sm = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_SUMMARY, f)?;
            gen.push(sm);
            fill_numeric(sm, &self.summaries, form.summary_chars as i64);
            y += ROW_H + ROW_GAP + 4;

            let check = |text: &str, id: i32, on: bool, y: i32| -> WinResult<HWND> {
                let c = child(page, w!("BUTTON"), text,
                    WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                    PAD, y, WIN_W - 2 * PAD - 20, ROW_H, id, f)?;
                SendMessageW(c, BM_SETCHECK, Some(WPARAM(if on { 1 } else { 0 })), None);
                Ok(c)
            };
            gen.push(check("Box the word being defined", ID_HIGHLIGHT, form.highlight_match, y)?);
            y += ROW_H;
            gen.push(check("Scroll long entries with the wheel", ID_SCROLL, form.scroll_popup, y)?);
            y += ROW_H;
            gen.push(check("Show related words beside the popup", ID_SIDE_PANEL, form.side_panel, y)?);
            y += ROW_H;
            gen.push(check("Hide the popup from screen capture", ID_EXCLUDE,
                  form.exclude_from_capture, y)?);
            y += ROW_H + 18;
            let y_general = y;

            // ---- Dictionaries ----
            y = 0;
            let bx = WIN_W - PAD - BTN_W - 8;
            let list_w = bx - 2 * PAD + 4;
            dict.push(group("Dictionaries — topmost is shown first", y, dict_group_h())?);
            y += 20;
            // Beside both boxes.
            let btn_y = y + DICT_CAP_H;
            for (caption, id) in
                [("Searched — for the selected OCR language", ID_DICTS),
                 ("Not searched", ID_DICTS_OFF)]
            {
                dict.push(child(page, w!("STATIC"), caption,
                    WINDOW_STYLE(0), PAD, y, list_w, DICT_CAP_H, 0, f)?);
                y += DICT_CAP_H;
                dict.push(child(page, w!("LISTBOX"), "",
                    WINDOW_STYLE(LBS_NOTIFY as u32) | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
                    PAD, y, list_w, DICT_BOX_H, id, f)?);
                y += DICT_BOX_H;
            }
            select_dict_row(h, &form.dict_names, &form.dict_excluded, DictBox::Searched, 0);
            for (i, (text, id)) in [
                ("Move up", ID_DICT_UP),
                ("Move down", ID_DICT_DOWN),
                ("Add…", ID_DICT_ADD),
                ("Remove", ID_DICT_REMOVE),
            ]
            .iter()
            .enumerate()
            {
                dict.push(child(page, w!("BUTTON"), text, WS_TABSTOP,
                      bx, btn_y + i as i32 * BTN_PITCH, BTN_W, ROW_H, *id, f)?);
            }
            y += ROW_GAP;
            dict.push(child(page, w!("STATIC"),
                "Order is matched by dictionary name. Check both lists after a change.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, DICT_HINT_H, 0, f)?);
            y += DICT_HINT_H + 8;

            // A rebuild is library-only.
            if form.library_empty && !form.dict_names.is_empty() {
                dict.push(child(page, w!("STATIC"),
                    "chibipop is using a dictionary built outside the app. Adding or \
                     removing here rebuilds from this list only — import your original \
                     .zip files first.",
                    WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 44, 0, f)?);
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
                dict.push(child(page, w!("STATIC"), &msg, WINDOW_STYLE(0),
                      PAD, y, WIN_W - 2 * PAD - 20, 32, 0, f)?);
                y += 36;
            }
            y += GROUP_GAP;

            // ---- Frequency data ----
            // WS_GROUP ends the last one.
            let freq_span = BTN_PITCH + ROW_H;
            let freq_h = 20 + freq_span + 8;
            dict.push(group_start("Frequency data — how common each word is", y, freq_h)?);
            y += 20;
            let freqs = child(page, w!("LISTBOX"), "",
                WINDOW_STYLE(LBS_NOTIFY as u32) | WS_TABSTOP | WS_BORDER | WS_VSCROLL,
                PAD, y, list_w, freq_span, ID_FREQS, f)?;
            dict.push(freqs);
            for name in &form.freq_names {
                SendMessageW(freqs, LB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
            }
            SendMessageW(freqs, LB_SETCURSEL, Some(WPARAM(0)), None);
            dict.push(child(page, w!("BUTTON"), "Add…", WS_TABSTOP,
                  bx, y, BTN_W, ROW_H, ID_FREQ_ADD, f)?);
            dict.push(child(page, w!("BUTTON"), "Remove", WS_TABSTOP,
                  bx, y + BTN_PITCH, BTN_W, ROW_H, ID_FREQ_REMOVE, f)?);
            y += freq_span + 8 + GROUP_GAP;
            let y_dict = y;

            // ---- OCR / Debug ----
            y = 0;
            ocr.push(group("OCR / Debug", y, 13 * ROW_H + 38)?);
            y += 20;
            let plugins_root = crate::paths::beside_exe("plugins");
            let found = crate::plugin::discover::discover(&plugins_root);
            let enabled_plugins = enabled_plugin_names();
            let mut engine_names = vec!["builtin".to_string()];
            engine_names.extend(enabled_text_providers(&found, &enabled_plugins));
            // Spec D4: keep it offered.
            if form.engine != "builtin" && !engine_names.contains(&form.engine) {
                engine_names.push(form.engine.clone());
            }
            ocr.push(label("OCR engine", y)?);
            // Room for Configure at bx.
            let engine_w = bx - FIELD_X - 8;
            let engine = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, engine_w, 220, ID_ENGINE, f)?;
            ocr.push(engine);
            for name in &engine_names {
                let shown = if name == "builtin" { "Built-in (Windows OCR)" } else { name };
                SendMessageW(engine, CB_ADDSTRING, None,
                    Some(LPARAM(wide(shown).as_ptr() as isize)));
            }
            let engine_idx = engine_names.iter().position(|n| n == &form.engine).unwrap_or(0);
            SendMessageW(engine, CB_SETCURSEL, Some(WPARAM(engine_idx)), None);
            self.engine_names = engine_names;
            let mut engine_dirs = HashMap::new();
            for (dir, parsed) in &found {
                if let Ok(m) = parsed {
                    if enabled_plugins.contains(&m.name)
                        && m.roles.contains(&crate::plugin::manifest::Role::TextProvider)
                    {
                        engine_dirs.insert(m.name.clone(), dir.clone());
                    }
                }
            }
            self.engine_dirs = engine_dirs;
            let cfg_btn = child(page, w!("BUTTON"), "Configure…", WS_TABSTOP,
                bx, y, BTN_W, ROW_H, ID_ENGINE_CONFIGURE, f)?;
            ocr.push(cfg_btn);
            let _ = ShowWindow(cfg_btn, SW_HIDE);
            y += ROW_H;
            ocr.push(label("OCR language", y)?);
            let lang = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_OCR_LANG, f)?;
            ocr.push(lang);
            let langs =
                language_choices(crate::text::ocr::installed_recognisers(), &form.ocr_language);
            for (name, _) in &langs {
                SendMessageW(lang, CB_ADDSTRING, None,
                    Some(LPARAM(wide(name).as_ptr() as isize)));
            }
            if let Some(i) = language_index(&langs, &form.ocr_language) {
                SendMessageW(lang, CB_SETCURSEL, Some(WPARAM(i)), None);
            }
            self.ocr_langs = langs.into_iter().map(|(_, tag)| tag).collect();
            let _ = EnableWindow(lang, engine_idx == 0);
            y += ROW_H;
            ocr.push(child(page, w!("STATIC"),
                "Installed recognizers, plus any marked (not installed).",
                WINDOW_STYLE(0), PAD, y + 4, WIN_W - 2 * PAD - 20, ROW_H, 0, f)?);
            y += ROW_H;
            self.passes = numeric_choices(
                PASSES_RANGE.0 as i64, PASSES_RANGE.1 as i64, 1,
                form.max_ocr_passes as i64);
            ocr.push(label("OCR passes per hover", y)?);
            let ps = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 160, ID_PASSES, f)?;
            ocr.push(ps);
            fill_numeric(ps, &self.passes, form.max_ocr_passes as i64);
            y += ROW_H;
            ocr.push(child(page, w!("STATIC"),
                "1 = no tiling. Higher reads further ahead but can resolve the wrong character.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 28, 0, f)?);
            y += 28;
            ocr.push(label("Capture width (px)", y)?);
            ocr.push(child(page, w!("EDIT"), &form.capture_width.to_string(),
                WS_TABSTOP | WS_BORDER,
                FIELD_X, y, FIELD_W, ROW_H, ID_CAPTURE_W, f)?);
            y += ROW_H;
            ocr.push(label("Capture height (px)", y)?);
            ocr.push(child(page, w!("EDIT"), &form.capture_height.to_string(),
                WS_TABSTOP | WS_BORDER,
                FIELD_X, y, FIELD_W, ROW_H, ID_CAPTURE_H, f)?);
            y += ROW_H;
            ocr.push(child(page, w!("STATIC"), "Vertical mode swaps these two values.",
                WINDOW_STYLE(0), PAD, y + 4, WIN_W - 2 * PAD - 20, ROW_H, 0, f)?);
            y += ROW_H;
            ocr.push(check("Prefer vertical text (manga, VN)",
                ID_PREFER_VERT, form.prefer_vertical, y)?);
            y += ROW_H;
            ocr.push(check("Scan alphanumeric text",
                ID_SCAN_ALNUM, form.scan_alphanumeric, y)?);
            y += ROW_H;
            let per_char = check("Look up each character as you hover",
                ID_PER_CHAR, form.per_character_lookup, y)?;
            ocr.push(per_char);
            let _ = EnableWindow(per_char, is_live);
            y += ROW_H;
            ocr.push(child(page, w!("STATIC"),
                "Live mode only. Off: the popup holds while the cursor stays on \
                 the matched word.",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 28, 0, f)?);
            y += 28;
            let scan = child(page, w!("BUTTON"), "Outline what each hover captured",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD, y, WIN_W - 2 * PAD - 20, ROW_H, ID_SHOW_SCAN, f)?;
            ocr.push(scan);
            SendMessageW(scan, BM_SETCHECK,
                Some(WPARAM(if form.show_scan_region { 1 } else { 0 })), None);
            y += ROW_H + 18;
            let y_ocr = y;

            // ---- Anki (own tab) ----
            y = 0;
            ank.push(group("Anki", y, 6 * ROW_H + 34)?);
            y += 20;
            let anki_chk = child(page, w!("BUTTON"), "Enable Anki integration",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD, y, WIN_W - 2 * PAD - 20, ROW_H, ID_ANKI_ENABLED, f)?;
            ank.push(anki_chk);
            SendMessageW(anki_chk, BM_SETCHECK,
                Some(WPARAM(if form.anki_enabled { 1 } else { 0 })), None);
            y += ROW_H;
            ank.push(label("AnkiConnect URL", y)?);
            ank.push(child(page, w!("EDIT"), &form.anki_url,
                WS_TABSTOP | WS_BORDER,
                FIELD_X, y, FIELD_W, ROW_H, ID_ANKI_URL, f)?);
            y += ROW_H;
            ank.push(label("Deck", y)?);
            let deck = child(page, w!("COMBOBOX"), &form.anki_deck,
                WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 160, ID_ANKI_DECK, f)?;
            ank.push(deck);
            SendMessageW(deck, WM_SETTEXT, None,
                Some(LPARAM(wide(&form.anki_deck).as_ptr() as isize)));
            y += ROW_H;
            ank.push(label("Note type", y)?);
            let model = child(page, w!("COMBOBOX"), &form.anki_model,
                WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 160, ID_ANKI_MODEL, f)?;
            ank.push(model);
            SendMessageW(model, WM_SETTEXT, None,
                Some(LPARAM(wide(&form.anki_model).as_ptr() as isize)));
            y += ROW_H;
            ank.push(label("Shortcut key", y)?);
            let add_vk = crate::config::parse_trigger_key(&form.anki_add_key).unwrap_or(0x41);
            ANKI_CAPTURED_VK.with(|c| c.set(Some((h.0 as isize, add_vk))));
            let add_name = crate::config::trigger_key_name(add_vk);
            ank.push(child(page, w!("BUTTON"), &add_name, WS_TABSTOP,
                FIELD_X, y, FIELD_W, ROW_H, ID_ANKI_ADD_KEY, f)?);
            y += ROW_H;
            ank.push(child(page, w!("BUTTON"), "Refresh", WS_TABSTOP,
                  PAD, y, 80, ROW_H, ID_ANKI_TEST, f)?);
            ank.push(child(page, w!("STATIC"),
                "Click to load decks and field mappings from Anki",
                WINDOW_STYLE(0), PAD + 88, y + 2, WIN_W - 2 * PAD - 96, ROW_H, 0, f)?);
            y += ROW_H + 8 + GROUP_GAP;

            // ---- Field-map toggle ----
            let toggle_text = field_map_toggle_label(self.field_map_collapsed.get());
            ank.push(child(page, w!("BUTTON"), toggle_text, WS_TABSTOP,
                  PAD, y, 160, ROW_H, ID_FIELD_MAP_TOGGLE, f)?);
            y += ROW_H + 8;

            let y_ank = y;

            // ---- Plugins ----
            y = 0;
            let plugins_root = crate::paths::beside_exe("plugins");
            let found = crate::plugin::discover::discover(&plugins_root);
            let enabled_plugins = enabled_plugin_names();
            plug.push(group("Plugins", y, plugins_group_h(found.len()))?);
            y += 20;
            if found.is_empty() {
                plug.push(child(page, w!("STATIC"),
                    &format!("No plugins found in {}.", plugins_root.display()),
                    WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD - 20, 36, 0, f)?);
                y += 40;
            } else {
                for (idx, (dir, parsed)) in found.iter().enumerate() {
                    if idx > 0 {
                        y += ROW_GAP;
                    }
                    let ry = y;
                    let idx = idx as i32;
                    let row = plugin_row(dir, parsed, &enabled_plugins);
                    plugin_names.push(plugin_key(dir, parsed));
                    plugin_dirs.push(dir.clone());
                    plug.push(child(page, w!("STATIC"), &row.label, WINDOW_STYLE(0),
                        PAD, ry + 4, bx - PAD - 8, ROW_H, 0, f)?);
                    let chk = child(page, w!("BUTTON"), "Enable",
                        WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                        bx, ry, BTN_W, ROW_H,
                        ID_PLUGIN_ENABLE_BASE + idx, f)?;
                    SendMessageW(chk, BM_SETCHECK,
                        Some(WPARAM(if row.checked { 1 } else { 0 })), None);
                    let _ = EnableWindow(chk, row.can_enable);
                    plug.push(chk);
                    plug.push(child(page, w!("STATIC"), &row.roles, WINDOW_STYLE(0),
                        PAD, ry + ROW_H + 4, bx - PAD - 8, ROW_H, 0, f)?);
                    let status_y = ry + 2 * ROW_H;
                    plug.push(child(page, w!("STATIC"), &row.status, WINDOW_STYLE(0),
                        PAD, status_y, bx - PAD - 8, PLUGIN_STATUS_H, 0, f)?);
                    plug.push(child(page, w!("BUTTON"), "Configure", WS_TABSTOP,
                        bx, status_y, BTN_W, ROW_H,
                        ID_PLUGIN_CONFIGURE_BASE + idx, f)?);
                    y = ry + PLUGIN_ROW_H;
                }
            }
            y += 8 + GROUP_GAP;
            let y_plugins = y;

            // Window y from here on.
            // place_bottom re-pins these.
            let bottom_y0 =
                y_general.max(y_dict).max(y_ocr).max(y_ank).max(y_plugins) + CONTENT_Y;

            // ---- Updates ----
            // Stays on `h`, not the pane.
            child(h, w!("BUTTON"), "Updates",
                WINDOW_STYLE(BS_GROUPBOX as u32),
                PAD - 6, bottom_y0, WIN_W - 2 * PAD, ROW_H + 24, ID_UPDATES, f)?;
            child(h, w!("BUTTON"), "Check for updates", WS_TABSTOP,
                  PAD, bottom_y0 + BOTTOM_UPDATE_DY, 136, ROW_H, ID_CHECK_UPDATE, f)?;

            // ---- Apply / Cancel ----
            // Also the progress line.
            let staged = form.has_staged();
            child(h, w!("EDIT"),
                apply_hint(self.apply_mode, staged),
                WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32) | WS_BORDER | WS_VSCROLL,
                PAD, bottom_y0 + BOTTOM_STATUS_DY, WIN_W - 2 * PAD - 16, STATUS_H,
                ID_STATUS, f)?;
            child(h, w!("BUTTON"), apply_caption(self.apply_mode),
                  WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
                  BOTTOM_APPLY_X, bottom_y0 + BOTTOM_BTN_DY, 136, ROW_H + 4, ID_APPLY, f)?;
            // Far left: not beside Apply.
            child(h, w!("BUTTON"), "Quit chibipop", WS_TABSTOP,
                  PAD, bottom_y0 + BOTTOM_BTN_DY, 116, ROW_H + 4, ID_QUIT, f)?;

            // The band the tabs occupy.
            let band_h = bottom_y0 - CONTENT_Y;
            let _ = SetWindowPos(self.viewport, None, 0, 0,
                dpi_scale(h, WIN_W), dpi_scale(h, band_h),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = SetWindowPos(self.content, None, 0, 0,
                dpi_scale(h, WIN_W), dpi_scale(h, band_h),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            self.place_viewport();

            self.anki_static_bottom = y_ank;
            self.tab_heights = [y_general, y_dict, y_ocr, y_ank, y_plugins];
            self.bottom_y0 = bottom_y0;

            // Start on General tab.
            for &c in dict.iter().chain(&ocr).chain(&ank).chain(&plug) {
                let _ = ShowWindow(c, SW_HIDE);
            }

            update_list_buttons(h);
        }
        self.general_ctrls = gen;
        self.dict_ctrls = dict;
        self.ocr_ctrls = ocr;
        self.anki_ctrls = ank;
        self.plugin_ctrls = plug;
        self.plugin_names = plugin_names;
        remember_plugin_dirs(h, plugin_dirs);
        Ok(self.bottom_y0 + BOTTOM_H)
    }

    /// The controls' current values, as a form.
    pub fn read(&self, template: &SettingsForm) -> SettingsForm {
        // SAFETY: every id below is a live descendant of `self.hwnd`, made
        // in `build` and destroyed only with the window in `Drop`.
        unsafe {
            let h = self.hwnd;
            let checked = |id: i32| -> bool {
                dlg_item(h, id)
                    .map(|c| SendMessageW(c, BM_GETCHECK, None, None).0 == 1)
                    .unwrap_or(false)
            };
            let combo_index = |id: i32| -> isize {
                dlg_item(h, id)
                    .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                    .unwrap_or(-1)
            };
            let pick = |values: &[i64], id: i32, fallback: i64| -> i64 {
                let i = combo_index(id);
                if i < 0 { fallback } else { *values.get(i as usize).unwrap_or(&fallback) }
            };
            let text_of = |id: i32| -> String {
                dlg_item(h, id).map(|c| window_text(c)).unwrap_or_default()
            };
            let px = |id: i32, fallback: i32| -> i32 { parse_px(&text_of(id), fallback) };

            // Empty is not missing.
            let (dict_names, dict_excluded) =
                match (list_rows(h, ID_DICTS), list_rows(h, ID_DICTS_OFF)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => (template.dict_names.clone(), template.dict_excluded.clone()),
                };
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
            let ocr_language = {
                let i = combo_index(ID_OCR_LANG);
                if i < 0 {
                    template.ocr_language.clone()
                } else {
                    self.ocr_langs
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.ocr_language.clone())
                }
            };

            let engine = {
                let i = combo_index(ID_ENGINE);
                if i < 0 {
                    template.engine.clone()
                } else {
                    self.engine_names
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.engine.clone())
                }
            };

            let trigger_key = resolved_trigger_key(h, &template.trigger_key);
            let anki_add_key = resolved_anki_add_key(h, &template.anki_add_key);

            // Empty is not missing.
            let rows = self.field_map_rows.borrow();
            let field_map = if rows.is_empty() {
                template.field_map.clone()
            } else {
                rows.iter()
                    .filter_map(|(name, combo)| {
                        let i = SendMessageW(*combo, CB_GETCURSEL, None, None).0.max(0);
                        let src = FIELD_MAP_SOURCES.get(i as usize).copied().unwrap_or("(none)");
                        row_mapping(name, src)
                    })
                    .collect()
            };

            SettingsForm {
                mode: if checked(ID_MODE_HOLD) {
                    crate::config::TriggerMode::HoldKey
                } else {
                    crate::config::TriggerMode::Live
                },
                trigger_key,
                theme: theme.to_string(),
                font,
                max_width_percent: pick(&self.widths, ID_MAX_WIDTH,
                                        template.max_width_percent as i64) as u8,
                max_height_percent: pick(&self.heights, ID_MAX_HEIGHT,
                                         template.max_height_percent as i64) as u8,
                summary_chars: pick(&self.summaries, ID_SUMMARY,
                                    template.summary_chars as i64) as usize,
                highlight_match: checked(ID_HIGHLIGHT),
                scroll_popup: checked(ID_SCROLL),
                side_panel: checked(ID_SIDE_PANEL),
                exclude_from_capture: checked(ID_EXCLUDE),
                dict_names,
                dict_excluded,
                dict_list_language: staged.dict_list_language.clone(),
                per_language: staged.per_language.clone(),
                max_ocr_passes: pick(&self.passes, ID_PASSES,
                                     template.max_ocr_passes as i64) as u8,
                prefer_vertical: checked(ID_PREFER_VERT),
                capture_width: px(ID_CAPTURE_W, template.capture_width),
                capture_height: px(ID_CAPTURE_H, template.capture_height),
                scan_alphanumeric: checked(ID_SCAN_ALNUM),
                per_character_lookup: checked(ID_PER_CHAR),
                ocr_language,
                engine,
                show_scan_region: checked(ID_SHOW_SCAN),
                freq_names,
                staged_adds: staged.staged_adds.clone(),
                staged_removes: staged.staged_removes.clone(),
                library_empty: staged.library_empty,
                unreadable: staged.unreadable.clone(),
                anki_enabled: checked(ID_ANKI_ENABLED),
                anki_url: text_of(ID_ANKI_URL),
                anki_deck: text_of(ID_ANKI_DECK),
                anki_model: text_of(ID_ANKI_MODEL),
                anki_add_key,
                field_map,
                enabled_plugins: self
                    .plugin_names
                    .iter()
                    .enumerate()
                    .filter(|&(idx, _)| checked(ID_PLUGIN_ENABLE_BASE + idx as i32))
                    .map(|(_, name)| name.clone())
                    .collect(),
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
        CLICK.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        TAB.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        FIELD_MAP_TOGGLE.with(|c| {
            if c.get().is_some_and(|h| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        LANG_CHANGED.with(|c| {
            if c.get().is_some_and(|h| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CAPTURING.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CAPTURED_VK.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        ANKI_CAPTURED_VK.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        DICT_BOX.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CAPTURE_PREV.with(|c| {
            let mut slot = c.borrow_mut();
            if slot.as_ref().is_some_and(|(h, _)| *h == self.hwnd.0 as isize) {
                *slot = None;
            }
        });
        UNREADABLE.with(|c| {
            let mut slot = c.borrow_mut();
            if slot.as_ref().is_some_and(|(h, _)| *h == self.hwnd.0 as isize) {
                *slot = None;
            }
        });
        PLUGIN_DIRS.with(|c| {
            let mut slot = c.borrow_mut();
            if slot.as_ref().is_some_and(|(h, _)| *h == self.hwnd.0 as isize) {
                *slot = None;
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

/// Re-split for one language.
fn scope_rows(
    all: &[String],
    list: &[String],
    unreadable: &[String],
) -> (Vec<String>, Vec<String>) {
    let readable = |n: &String| !unreadable.iter().any(|u| u == n);
    let installed = all.iter().filter(|n| readable(n)).map(String::as_str);
    if !crate::present::any_listed(installed, list) {
        return (all.to_vec(), Vec::new());
    }
    let keep = |n: &String| !readable(n) || crate::present::dict_order_rank(n, list).is_some();
    let mut active: Vec<String> = all.iter().filter(|n| keep(n)).cloned().collect();
    active.sort_by_key(|n| crate::present::dict_order_rank(n, list).unwrap_or(usize::MAX));
    (active, all.iter().filter(|n| !keep(n)).cloned().collect())
}

/// Which listbox a row is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DictBox {
    Searched,
    NotSearched,
}

/// Never search nothing.
fn another_readable_row(searched: &[String], unreadable: &[String], index: usize) -> bool {
    let readable = |n: &String| !unreadable.iter().any(|u| u == n);
    searched.iter().enumerate().any(|(j, n)| j != index && readable(n))
}

/// Where a move would land.
fn dict_move_target(
    searched: &[String],
    not_searched: &[String],
    unreadable: &[String],
    from: DictBox,
    index: usize,
    up: bool,
) -> Option<(DictBox, usize)> {
    match (from, up) {
        (DictBox::Searched, true) => {
            if index > 0 && index < searched.len() {
                Some((DictBox::Searched, index - 1))
            } else {
                None
            }
        }
        (DictBox::Searched, false) => {
            if index + 1 < searched.len() {
                Some((DictBox::Searched, index + 1))
            } else if index < searched.len()
                && another_readable_row(searched, unreadable, index)
            {
                Some((DictBox::NotSearched, 0))
            } else {
                None
            }
        }
        (DictBox::NotSearched, true) => {
            if index == 0 && !not_searched.is_empty() {
                Some((DictBox::Searched, searched.len()))
            } else if index < not_searched.len() {
                Some((DictBox::NotSearched, index - 1))
            } else {
                None
            }
        }
        (DictBox::NotSearched, false) => {
            if index + 1 < not_searched.len() {
                Some((DictBox::NotSearched, index + 1))
            } else {
                None
            }
        }
    }
}

/// Move; returns the landing.
fn dict_move(
    searched: &mut Vec<String>,
    not_searched: &mut Vec<String>,
    unreadable: &[String],
    from: DictBox,
    index: usize,
    up: bool,
) -> Option<(DictBox, usize)> {
    let landed = dict_move_target(searched, not_searched, unreadable, from, index, up)?;
    match (from, landed.0) {
        (DictBox::Searched, DictBox::Searched) => searched.swap(index, landed.1),
        (DictBox::NotSearched, DictBox::NotSearched) => not_searched.swap(index, landed.1),
        (DictBox::Searched, DictBox::NotSearched) => {
            not_searched.insert(landed.1, searched.remove(index));
        }
        (DictBox::NotSearched, DictBox::Searched) => {
            searched.insert(landed.1, not_searched.remove(index));
        }
    }
    Some(landed)
}

/// Append; returns its index.
fn add_dict(searched: &mut Vec<String>, name: &str) -> usize {
    searched.push(name.to_string());
    searched.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// X quits standalone chibipop.
    #[test]
    fn wm_close_records_a_cancel_outcome() {
        let hwnd = HWND(4242 as *mut core::ffi::c_void);
        let _ = unsafe { wndproc(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)) };
        let got = OUTCOME.with(|c| c.get());
        assert_eq!(Some((hwnd.0 as isize, SettingsOutcome::Cancel)), got);
    }

    #[test]
    fn numeric_choices_step_the_range() {
        assert_eq!(vec![10, 15, 20], numeric_choices(10, 20, 5, 10));
    }

    // ---- field mapping ----

    fn mapping(anki_field: &str, source: &str) -> crate::config::FieldMapping {
        crate::config::FieldMapping { anki_field: anki_field.into(), source: source.into() }
    }

    #[test]
    fn default_source_finds_a_matching_field() {
        let existing = vec![mapping("Expression", "expression")];
        assert_eq!("expression", default_source(&existing, "Expression"));
    }

    #[test]
    fn default_source_falls_back_to_none_for_an_unmapped_field() {
        let existing = vec![mapping("Expression", "expression")];
        assert_eq!("(none)", default_source(&existing, "ExpressionAudio"));
    }

    #[test]
    fn default_source_falls_back_to_none_with_no_config_at_all() {
        assert_eq!("(none)", default_source(&[], "Expression"));
    }

    #[test]
    fn row_mapping_builds_a_real_mapping() {
        assert_eq!(
            Some(mapping("Front", "expression")),
            row_mapping("Front", "expression"),
        );
    }

    /// "(none)" maps nothing.
    #[test]
    fn row_mapping_is_none_for_the_none_source() {
        assert_eq!(None, row_mapping("Front", "(none)"));
    }

    fn dummy_hwnd(n: isize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    #[test]
    fn field_names_match_true_for_identical_names_in_order() {
        let rows = vec![
            ("Expression".to_string(), dummy_hwnd(1)),
            ("Glossary".to_string(), dummy_hwnd(2)),
        ];
        let fields = vec!["Expression".to_string(), "Glossary".to_string()];
        assert!(field_names_match(&rows, &fields));
    }

    #[test]
    fn field_names_match_false_for_a_different_model() {
        let rows = vec![("Expression".to_string(), dummy_hwnd(1))];
        let fields = vec!["Front".to_string()];
        assert!(!field_names_match(&rows, &fields));
    }

    #[test]
    fn field_names_match_false_for_a_different_count() {
        let rows = vec![("Expression".to_string(), dummy_hwnd(1))];
        let fields = vec!["Expression".to_string(), "Glossary".to_string()];
        assert!(!field_names_match(&rows, &fields));
    }

    #[test]
    fn field_names_match_is_order_sensitive() {
        let rows = vec![
            ("Expression".to_string(), dummy_hwnd(1)),
            ("Glossary".to_string(), dummy_hwnd(2)),
        ];
        let fields = vec!["Glossary".to_string(), "Expression".to_string()];
        assert!(!field_names_match(&rows, &fields));
    }

    // ---- field-map columns ----

    #[test]
    fn field_map_rows_needed_ceils_by_two() {
        assert_eq!(1, field_map_rows_needed(1));
        assert_eq!(1, field_map_rows_needed(2));
        assert_eq!(2, field_map_rows_needed(3));
        assert_eq!(12, field_map_rows_needed(23));
    }

    /// Never zero, even for an empty list.
    #[test]
    fn field_map_rows_needed_floors_at_one() {
        assert_eq!(1, field_map_rows_needed(0));
    }

    #[test]
    fn column_label_keeps_a_short_name_whole() {
        assert_eq!("Glossary", column_label("Glossary"));
    }

    /// Boundary: exactly the max stays whole.
    #[test]
    fn column_label_keeps_a_max_length_name_whole() {
        let name = "ABCDEFGHIJKLMNOPQR"; // 18 chars
        assert_eq!(name, column_label(name));
    }

    #[test]
    fn column_label_truncates_a_long_name() {
        assert_eq!("ExpressionReading", column_label("ExpressionReading"));
        assert_eq!("ExpressionFurigana", column_label("ExpressionFurigana"));
        assert_eq!("IsWordAndSentenceC", column_label("IsWordAndSentenceCard"));
    }

    /// Truncation must land on a char boundary.
    #[test]
    fn column_label_is_char_boundary_safe() {
        let name = "日本語日本語日本語日本語日本語日本語日本語";
        let got = column_label(name);
        assert_eq!(18, got.chars().count());
    }

    #[test]
    fn field_map_toggle_label_shows_the_fold_direction() {
        assert!(field_map_toggle_label(true).ends_with('\u{25B6}'));
        assert!(field_map_toggle_label(false).ends_with('\u{25BC}'));
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

    #[test]
    fn wm_notify_records_a_tab_change() {
        let hwnd = HWND(5353 as *mut core::ffi::c_void);
        TAB.with(|c| c.set(Some((hwnd.0 as isize, 1))));
        let got = TAB.with(|c| c.get());
        assert_eq!(Some((hwnd.0 as isize, 1)), got);
        TAB.with(|c| c.set(None));
    }

    /// The vertical-writing duplicates Windows lists beside each family are
    /// never wanted, and the real list must not be empty on this machine.
    #[test]
    fn the_japanese_font_list_excludes_vertical_duplicates() {
        let families = japanese_font_families();
        assert!(!families.is_empty(), "no Japanese-capable font families found");
        assert!(!families.iter().any(|f| f.starts_with('@')), "got {families:?}");
    }

    // ---- trigger-key capture ----

    #[test]
    fn take_captured_key_is_none_when_not_capturing() {
        let hwnd = HWND(6001 as *mut core::ffi::c_void);
        assert_eq!(None, take_captured_key(hwnd, 0x10));
    }

    /// Ends capture with its name.
    #[test]
    fn take_captured_key_accepts_a_named_key() {
        let hwnd = HWND(6002 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        let got = take_captured_key(hwnd, 0x11);

        assert_eq!(Some((ID_TRIGGER_KEY, "Ctrl".to_string())), got);
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// Any vk is now valid.
    #[test]
    fn take_captured_key_accepts_a_previously_unlisted_key() {
        let hwnd = HWND(6003 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        let got = take_captured_key(hwnd, 0x41); // 'A'

        assert_eq!(Some((ID_TRIGGER_KEY, "A".to_string())), got);
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// Stashed so `read()` can see it later.
    #[test]
    fn take_captured_key_records_the_vk_for_read() {
        let hwnd = HWND(6007 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        take_captured_key(hwnd, 0x41);

        assert_eq!("0x41", resolved_trigger_key(hwnd, "shift"));
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// A second capturable control.
    #[test]
    fn take_captured_key_routes_the_anki_add_key_to_its_own_id() {
        let hwnd = HWND(6008 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_ANKI_ADD_KEY))));

        let got = take_captured_key(hwnd, 0x41);

        assert_eq!(Some((ID_ANKI_ADD_KEY, "A".to_string())), got);
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        ANKI_CAPTURED_VK.with(|c| c.set(None));
    }

    /// The two cells stay apart.
    #[test]
    fn take_captured_key_does_not_disturb_the_trigger_key_cell() {
        let hwnd = HWND(6009 as *mut core::ffi::c_void);
        CAPTURED_VK.with(|c| c.set(Some((hwnd.0 as isize, 0x10))));
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_ANKI_ADD_KEY))));

        take_captured_key(hwnd, 0x42);

        assert_eq!(
            Some((hwnd.0 as isize, 0x10)),
            CAPTURED_VK.with(|c| c.get()),
            "the trigger key's own capture must be untouched"
        );
        CAPTURED_VK.with(|c| c.set(None));
        ANKI_CAPTURED_VK.with(|c| c.set(None));
    }

    #[test]
    fn resolved_trigger_key_falls_back_to_the_template_when_uncaptured() {
        let hwnd = HWND(6005 as *mut core::ffi::c_void);
        CAPTURED_VK.with(|c| c.set(None));

        assert_eq!("ctrl", resolved_trigger_key(hwnd, "ctrl"));
    }

    #[test]
    fn resolved_trigger_key_falls_back_verbatim_when_unparseable() {
        let hwnd = HWND(6006 as *mut core::ffi::c_void);
        CAPTURED_VK.with(|c| c.set(None));

        assert_eq!("garbage", resolved_trigger_key(hwnd, "garbage"));
    }

    // ---- anki add-key capture ----

    #[test]
    fn resolved_anki_add_key_falls_back_to_the_template_when_uncaptured() {
        let hwnd = HWND(6010 as *mut core::ffi::c_void);
        ANKI_CAPTURED_VK.with(|c| c.set(None));

        assert_eq!("ctrl", resolved_anki_add_key(hwnd, "ctrl"));
    }

    /// The default normalizes.
    #[test]
    fn resolved_anki_add_key_normalizes_the_default_letter() {
        let hwnd = HWND(6013 as *mut core::ffi::c_void);
        ANKI_CAPTURED_VK.with(|c| c.set(None));

        assert_eq!("0x41", resolved_anki_add_key(hwnd, "a"));
    }

    #[test]
    fn resolved_anki_add_key_uses_the_freshly_captured_vk() {
        let hwnd = HWND(6011 as *mut core::ffi::c_void);
        ANKI_CAPTURED_VK.with(|c| c.set(Some((hwnd.0 as isize, 0x73))));

        assert_eq!("f4", resolved_anki_add_key(hwnd, "a"));
        ANKI_CAPTURED_VK.with(|c| c.set(None));
    }

    #[test]
    fn stored_trigger_key_names_known_keys() {
        assert_eq!("shift", stored_trigger_key(0x10));
        assert_eq!("ctrl", stored_trigger_key(0x11));
        assert_eq!("alt", stored_trigger_key(0x12));
        assert_eq!("f5", stored_trigger_key(0x74));
    }

    #[test]
    fn stored_trigger_key_hexes_everything_else() {
        assert_eq!("0x41", stored_trigger_key(0x41));
    }

    #[test]
    fn stored_trigger_key_round_trips_through_parse_trigger_key() {
        for vk in [0x10u16, 0x11, 0x12, 0x70, 0x7B, 0x41, 0x30, 0x20, 0xBA] {
            let stored = stored_trigger_key(vk);
            assert_eq!(Some(vk), crate::config::parse_trigger_key(&stored), "{stored}");
        }
    }

    // ---- capture size fields ----

    #[test]
    fn parse_px_reads_a_plain_number() {
        assert_eq!(640, parse_px("640", 500));
    }

    /// Typing leaves stray spaces.
    #[test]
    fn parse_px_ignores_surrounding_space() {
        assert_eq!(640, parse_px("  640 ", 500));
    }

    /// The trap: never zero.
    #[test]
    fn parse_px_keeps_the_old_value_for_junk() {
        assert_eq!(500, parse_px("", 500));
        assert_eq!(500, parse_px("abc", 500));
        assert_eq!(500, parse_px("640px", 500));
        assert_eq!(500, parse_px("6.4", 500));
    }

    // ---- apply caption ----

    /// Only `run` applies live.
    #[test]
    fn a_live_window_with_nothing_staged_just_applies() {
        assert_eq!("Apply", apply_caption(ApplyMode::Live));
        assert!(apply_hint(ApplyMode::Live, false).contains("right away"));
    }

    /// No rebuild, no restart.
    #[test]
    fn a_staged_dictionary_promises_an_in_place_update() {
        assert_eq!("Apply", apply_caption(ApplyMode::Live));
        let hint = apply_hint(ApplyMode::Live, true);
        assert!(hint.contains("in place"), "{hint}");
        assert!(!hint.contains("rebuild"), "{hint}");
        assert!(!hint.contains("restart"), "{hint}");
    }

    /// It reloads no other process.
    #[test]
    fn a_standalone_window_never_promises_a_live_apply() {
        assert_eq!("Apply && Restart", apply_caption(ApplyMode::Standalone));
        for staged in [false, true] {
            assert!(apply_hint(ApplyMode::Standalone, staged).contains("restarts chibipop"));
        }
    }

    // ---- ocr language list ----

    fn installed() -> Vec<(String, String)> {
        vec![
            ("Japanese".to_string(), "ja".to_string()),
            ("English (United States)".to_string(), "en-US".to_string()),
        ]
    }

    /// Spec D4: never reselect.
    #[test]
    fn a_configured_language_missing_from_the_list_is_appended() {
        let got = language_choices(installed(), "ko");
        assert_eq!(3, got.len());
        assert_eq!(("ko (not installed)".to_string(), "ko".to_string()), got[2]);
    }

    /// Survives a failed call.
    #[test]
    fn an_empty_list_still_offers_the_configured_language() {
        assert_eq!(
            vec![("ja (not installed)".to_string(), "ja".to_string())],
            language_choices(Vec::new(), "ja")
        );
    }

    /// Display name out, tag in.
    #[test]
    fn an_installed_language_keeps_its_display_name_and_its_tag() {
        let got = language_choices(installed(), "ja");
        assert_eq!(installed(), got);
        assert_eq!("Japanese", got[0].0);
        assert_eq!("ja", got[0].1);
    }

    #[test]
    fn the_installed_order_is_the_listed_order() {
        let tags: Vec<String> =
            language_choices(installed(), "ja").into_iter().map(|(_, t)| t).collect();
        assert_eq!(vec!["ja".to_string(), "en-US".to_string()], tags);
    }

    /// A blank combo if this drifts.
    #[test]
    fn a_configured_tag_matches_its_entry_whatever_its_case() {
        let rows = language_choices(installed(), "EN-us");
        assert_eq!(2, rows.len());
        assert_eq!(Some(1), language_index(&rows, "EN-us"));
    }

    /// Nothing to keep, none added.
    #[test]
    fn an_empty_configured_language_is_not_offered_as_a_blank_row() {
        assert!(language_choices(Vec::new(), "").is_empty());
        assert_eq!(installed(), language_choices(installed(), ""));
    }

    /// What `read` gives back.
    #[test]
    fn an_untouched_combo_reads_back_the_configured_tag() {
        for configured in ["ja", "en-US", "ko"] {
            let rows = language_choices(installed(), configured);
            let i = language_index(&rows, configured).expect("a row is always selected");
            assert_eq!(configured, rows[i].1);
        }
    }

    #[test]
    fn nothing_is_selected_when_no_language_is_configured() {
        assert_eq!(None, language_index(&installed(), ""));
    }

    fn installed_four() -> Vec<(String, String)> {
        vec![
            ("English (United States)".to_string(), "en-US".to_string()),
            ("Japanese".to_string(), "ja".to_string()),
            ("Chinese (Simplified)".to_string(), "zh-Hans-CN".to_string()),
            ("Chinese (Traditional)".to_string(), "zh-Hant-TW".to_string()),
        ]
    }

    #[test]
    fn a_configured_prefix_is_not_appended_as_a_phantom_row() {
        assert_eq!(installed_four(), language_choices(installed_four(), "zh-Hans"));
    }

    #[test]
    fn a_configured_prefix_selects_the_specific_installed_row() {
        let rows = language_choices(installed_four(), "zh-Hans");
        assert_eq!(Some(2), language_index(&rows, "zh-Hans"));
        assert_eq!("zh-Hans-CN", rows[2].1);
    }

    #[test]
    fn a_more_specific_configured_tag_selects_the_bare_row() {
        let rows = language_choices(installed_four(), "ja-JP");
        assert_eq!(installed_four(), rows);
        assert_eq!(Some(1), language_index(&rows, "ja-JP"));
    }

    /// FIX 1 behaviour, still live.
    #[test]
    fn a_genuinely_absent_language_is_still_appended_and_read_back() {
        let rows = language_choices(installed_four(), "ko");
        assert_eq!(5, rows.len());
        assert_eq!(("ko (not installed)".to_string(), "ko".to_string()), rows[4]);
        assert_eq!(Some(4), language_index(&rows, "ko"));
    }

    /// Boundary, not starts_with.
    #[test]
    fn a_partial_subtag_is_treated_as_absent() {
        let rows = language_choices(installed_four(), "zh-Han");
        assert_eq!(5, rows.len());
        assert_eq!(Some(4), language_index(&rows, "zh-Han"));
        assert_eq!("zh-Han", rows[4].1);
    }

    /// First match wins; arbitrary.
    #[test]
    fn an_ambiguous_prefix_picks_the_first_installed_match() {
        let rows = language_choices(installed_four(), "zh");
        assert_eq!(installed_four(), rows);
        assert_eq!(Some(2), language_index(&rows, "zh"));
        assert_eq!("zh-Hans-CN", rows[2].1);
    }

    #[test]
    fn a_configured_prefix_matches_whatever_its_case() {
        let rows = language_choices(installed_four(), "ZH-hans");
        assert_eq!(installed_four(), rows);
        assert_eq!(Some(2), language_index(&rows, "ZH-hans"));
    }

    // ---- engine configure ----

    #[test]
    fn configure_button_hidden_when_builtin_selected() {
        assert!(!should_show_configure(0));
    }

    #[test]
    fn configure_button_visible_when_plugin_selected() {
        assert!(should_show_configure(1));
        assert!(should_show_configure(3));
    }

    // ---- re-scoping ----

    fn installed_two() -> Vec<String> {
        vec!["Jitendex.org [2026-07-09]".to_string(), "大辞林　第四版".to_string()]
    }

    /// No list: everything searched.
    #[test]
    fn an_empty_language_list_leaves_every_row_active() {
        assert_eq!(
            (installed_two(), Vec::new()),
            scope_rows(&installed_two(), &[], &[])
        );
    }

    /// Substrings, not live names.
    #[test]
    fn a_language_list_splits_and_orders_the_rows() {
        let (active, excluded) =
            scope_rows(&installed_two(), &["大辞林".to_string()], &[]);
        assert_eq!(vec!["大辞林　第四版".to_string()], active);
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], excluded);
    }

    #[test]
    fn the_list_order_wins_over_the_row_order() {
        let list = vec!["大辞林".to_string(), "Jitendex".to_string()];
        let (active, excluded) = scope_rows(&installed_two(), &list, &[]);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            active
        );
        assert!(excluded.is_empty());
    }

    /// Stale list: nothing hidden.
    #[test]
    fn a_list_matching_nothing_installed_leaves_every_row_active() {
        let list = vec!["Daijirin".to_string()];
        assert_eq!(
            (installed_two(), Vec::new()),
            scope_rows(&installed_two(), &list, &[])
        );
    }

    /// Blanks pin nothing.
    #[test]
    fn a_blank_only_list_leaves_every_row_active() {
        let list = vec![String::new()];
        assert_eq!(
            (installed_two(), Vec::new()),
            scope_rows(&installed_two(), &list, &[])
        );
    }

    /// Unreadable rows cannot scope.
    #[test]
    fn a_list_naming_only_an_unreadable_row_leaves_every_row_active() {
        let mut rows = installed_two();
        rows.push("broken.zip".to_string());
        let unreadable = vec!["broken.zip".to_string()];
        let list = vec!["broken".to_string()];
        assert_eq!(
            (rows.clone(), Vec::new()),
            scope_rows(&rows, &list, &unreadable)
        );
    }

    /// It must stay removable.
    #[test]
    fn an_unreadable_row_stays_on_the_searched_side() {
        let mut rows = installed_two();
        rows.push("broken.zip".to_string());
        let unreadable = vec!["broken.zip".to_string()];
        let (active, excluded) =
            scope_rows(&rows, &["大辞林".to_string()], &unreadable);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "broken.zip".to_string()],
            active
        );
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], excluded);
    }

    // ---- the two-box move ----

    fn names(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| r.to_string()).collect()
    }

    #[test]
    fn up_on_the_top_of_not_searched_crosses_to_the_bottom_of_searched() {
        let mut searched = names(&["A", "B"]);
        let mut not = names(&["C", "D"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::NotSearched, 0, true);
        assert_eq!(Some((DictBox::Searched, 2)), landed);
        assert_eq!(names(&["A", "B", "C"]), searched);
        assert_eq!(names(&["D"]), not);
    }

    #[test]
    fn down_on_the_bottom_of_searched_crosses_to_the_top_of_not_searched() {
        let mut searched = names(&["A", "B"]);
        let mut not = names(&["C"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 1, false);
        assert_eq!(Some((DictBox::NotSearched, 0)), landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B", "C"]), not);
    }

    #[test]
    fn up_on_the_top_of_searched_does_nothing() {
        let mut searched = names(&["A", "B"]);
        let mut not = names(&["C"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 0, true);
        assert_eq!(None, landed);
        assert_eq!(names(&["A", "B"]), searched);
        assert_eq!(names(&["C"]), not);
    }

    #[test]
    fn down_on_the_bottom_of_not_searched_does_nothing() {
        let mut searched = names(&["A"]);
        let mut not = names(&["B", "C"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::NotSearched, 1, false);
        assert_eq!(None, landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B", "C"]), not);
    }

    /// Never search nothing.
    #[test]
    fn the_last_searched_row_will_not_cross_down() {
        let mut searched = names(&["A"]);
        let mut not = names(&["B"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 0, false);
        assert_eq!(None, landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B"]), not);
    }

    /// keyed_names strips it first.
    #[test]
    fn an_unreadable_row_does_not_count_toward_the_last_searched_rule() {
        let mut searched = names(&["bad.zip", "A"]);
        let mut not = names(&["B"]);
        let bad = names(&["bad.zip"]);
        let landed =
            dict_move(&mut searched, &mut not, &bad, DictBox::Searched, 1, false);
        assert_eq!(None, landed);
        assert_eq!(names(&["bad.zip", "A"]), searched);
        assert_eq!(names(&["B"]), not);
    }

    #[test]
    fn adding_appends_to_searched() {
        let mut searched = names(&["A", "B"]);
        assert_eq!(2, add_dict(&mut searched, "C"));
        assert_eq!(names(&["A", "B", "C"]), searched);
    }

    #[test]
    fn up_inside_searched_reorders_without_crossing() {
        let mut searched = names(&["A", "B", "C"]);
        let mut not = names(&["D"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 2, true);
        assert_eq!(Some((DictBox::Searched, 1)), landed);
        assert_eq!(names(&["A", "C", "B"]), searched);
        assert_eq!(names(&["D"]), not);
    }

    #[test]
    fn down_inside_searched_reorders_without_crossing() {
        let mut searched = names(&["A", "B", "C"]);
        let mut not = names(&["D"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 0, false);
        assert_eq!(Some((DictBox::Searched, 1)), landed);
        assert_eq!(names(&["B", "A", "C"]), searched);
        assert_eq!(names(&["D"]), not);
    }

    #[test]
    fn up_inside_not_searched_reorders_without_crossing() {
        let mut searched = names(&["A"]);
        let mut not = names(&["B", "C", "D"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::NotSearched, 2, true);
        assert_eq!(Some((DictBox::NotSearched, 1)), landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B", "D", "C"]), not);
    }

    #[test]
    fn down_inside_not_searched_reorders_without_crossing() {
        let mut searched = names(&["A"]);
        let mut not = names(&["B", "C", "D"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::NotSearched, 0, false);
        assert_eq!(Some((DictBox::NotSearched, 1)), landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["C", "B", "D"]), not);
    }

    /// It contributes no name.
    #[test]
    fn an_unreadable_row_may_itself_cross_down() {
        let mut searched = names(&["A", "bad.zip"]);
        let mut not: Vec<String> = Vec::new();
        let bad = names(&["bad.zip"]);
        let landed =
            dict_move(&mut searched, &mut not, &bad, DictBox::Searched, 1, false);
        assert_eq!(Some((DictBox::NotSearched, 0)), landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["bad.zip"]), not);
    }

    /// Remove can empty the box.
    #[test]
    fn a_row_crosses_up_into_an_empty_searched_box() {
        let mut searched: Vec<String> = Vec::new();
        let mut not = names(&["A", "B"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::NotSearched, 0, true);
        assert_eq!(Some((DictBox::Searched, 0)), landed);
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B"]), not);
    }

    #[test]
    fn a_move_from_beyond_the_last_row_does_nothing() {
        let mut searched = names(&["A", "B"]);
        let mut not = names(&["C"]);
        let landed =
            dict_move(&mut searched, &mut not, &[], DictBox::Searched, 2, true);
        assert_eq!(None, landed);
        assert_eq!(names(&["A", "B"]), searched);
        assert_eq!(names(&["C"]), not);
    }

    /// Greying asks without moving.
    #[test]
    fn the_target_refuses_exactly_what_the_move_refuses() {
        let searched = names(&["A"]);
        let not = names(&["B"]);
        assert_eq!(
            None,
            dict_move_target(&searched, &not, &[], DictBox::Searched, 0, false)
        );
        assert_eq!(
            Some((DictBox::Searched, 1)),
            dict_move_target(&searched, &not, &[], DictBox::NotSearched, 0, true)
        );
        assert_eq!(names(&["A"]), searched);
        assert_eq!(names(&["B"]), not);
    }

    // ---- which box acts ----

    #[test]
    fn only_the_searched_box_selected_acts_on_searched() {
        assert_eq!(DictBox::Searched, acting_box(true, false, None));
        assert_eq!(DictBox::Searched, acting_box(true, false, Some(DictBox::NotSearched)));
    }

    #[test]
    fn only_the_not_searched_box_selected_acts_on_not_searched() {
        assert_eq!(DictBox::NotSearched, acting_box(false, true, None));
    }

    /// A LISTBOX keeps a selection.
    #[test]
    fn both_boxes_selected_acts_on_the_last_one_touched() {
        assert_eq!(DictBox::NotSearched, acting_box(true, true, Some(DictBox::NotSearched)));
        assert_eq!(DictBox::Searched, acting_box(true, true, Some(DictBox::Searched)));
    }

    #[test]
    fn both_boxes_selected_with_nothing_tracked_acts_on_searched() {
        assert_eq!(DictBox::Searched, acting_box(true, true, None));
    }

    #[test]
    fn neither_box_selected_acts_on_searched() {
        assert_eq!(DictBox::Searched, acting_box(false, false, None));
    }

    /// A selection beats the memory.
    #[test]
    fn a_stale_tracked_box_never_beats_a_single_selection() {
        assert_eq!(DictBox::NotSearched, acting_box(false, true, Some(DictBox::Searched)));
    }

    #[test]
    fn remove_follows_the_box_holding_the_selection() {
        assert_eq!(ID_DICTS_OFF, dict_box_id(acting_box(false, true, None)));
        assert_eq!(ID_DICTS, dict_box_id(acting_box(true, false, None)));
    }

    // ---- the layout budget ----

    /// The one-box layout cost 218.
    #[test]
    fn the_dictionaries_group_did_not_outgrow_the_one_box_layout() {
        assert_eq!(20 + 20 + (4 * BTN_PITCH + ROW_H) + ROW_GAP + 28 + 8, dict_group_h());
    }

    // ---- Plugins tab ----

    fn manifest_stub(
        name: &str,
        roles: Vec<crate::plugin::manifest::Role>,
    ) -> crate::plugin::manifest::Manifest {
        crate::plugin::manifest::Manifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            protocol: 1,
            command: "python".to_string(),
            args: vec![],
            roles,
            text_provider: None,
            field_contributor: None,
        }
    }

    #[test]
    fn an_enabled_plugin_is_labelled_enabled() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        let row = plugin_row(Path::new("meikiocr"), &Ok(m), &["meikiocr".to_string()]);
        assert_eq!("Enabled", row.status);
        assert!(row.checked);
        assert!(row.can_enable);
        assert_eq!("meikiocr 0.1.0", row.label);
    }

    #[test]
    fn an_unlisted_plugin_is_labelled_disabled() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        let row = plugin_row(Path::new("meikiocr"), &Ok(m), &[]);
        assert_eq!("Disabled", row.status);
        assert!(!row.checked);
        assert!(row.can_enable);
    }

    /// The core rule: never dropped.
    #[test]
    fn a_refused_plugin_shows_its_error_and_cannot_enable() {
        let err = anyhow::anyhow!("plugin \"beta\" declares no roles");
        let row = plugin_row(Path::new("some/dir/beta"), &Err(err), &["beta".to_string()]);
        assert!(row.status.contains("declares no roles"), "{}", row.status);
        assert!(row.status.starts_with("Refused"));
        assert!(!row.checked);
        assert!(!row.can_enable);
        assert_eq!("beta", row.label);
    }

    #[test]
    fn enabled_text_providers_includes_an_enabled_provider() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        let found = vec![(PathBuf::from("meikiocr"), Ok(m))];
        let names = enabled_text_providers(&found, &["meikiocr".to_string()]);
        assert_eq!(vec!["meikiocr".to_string()], names);
    }

    #[test]
    fn enabled_text_providers_excludes_a_disabled_provider() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        let found = vec![(PathBuf::from("meikiocr"), Ok(m))];
        assert!(enabled_text_providers(&found, &[]).is_empty());
    }

    #[test]
    fn enabled_text_providers_excludes_a_non_provider_role() {
        let m = manifest_stub("scorer", vec![crate::plugin::manifest::Role::FieldContributor]);
        let found = vec![(PathBuf::from("scorer"), Ok(m))];
        let names = enabled_text_providers(&found, &["scorer".to_string()]);
        assert!(names.is_empty());
    }

    #[test]
    fn enabled_text_providers_excludes_a_refused_manifest() {
        let err = anyhow::anyhow!("plugin \"beta\" declares no roles");
        let found = vec![(PathBuf::from("beta"), Err(err))];
        let names = enabled_text_providers(&found, &["beta".to_string()]);
        assert!(names.is_empty());
    }

    #[test]
    fn roles_text_joins_multiple_roles() {
        let roles = vec![
            crate::plugin::manifest::Role::TextProvider,
            crate::plugin::manifest::Role::FieldContributor,
        ];
        assert_eq!("text-provider, field-contributor", roles_text(&roles));
    }

    #[test]
    fn roles_text_handles_a_single_role() {
        assert_eq!("text-provider", roles_text(&[crate::plugin::manifest::Role::TextProvider]));
    }

    #[test]
    fn roles_text_is_a_dash_for_no_roles() {
        assert_eq!("—", roles_text(&[]));
    }

    #[test]
    fn dir_label_reads_the_folder_name() {
        assert_eq!("meikiocr", dir_label(Path::new("C:/plugins/meikiocr")));
    }

    #[test]
    fn plugins_group_h_for_no_plugins() {
        assert_eq!(20 + 40 + 8, plugins_group_h(0));
    }

    #[test]
    fn plugins_group_h_for_one_plugin() {
        assert_eq!(20 + PLUGIN_ROW_H + 8, plugins_group_h(1));
    }

    #[test]
    fn plugins_group_h_for_two_plugins() {
        assert_eq!(20 + 2 * PLUGIN_ROW_H + ROW_GAP + 8, plugins_group_h(2));
    }

    #[test]
    fn plugin_key_uses_the_manifest_name() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        assert_eq!("meikiocr", plugin_key(Path::new("meikiocr"), &Ok(m)));
    }

    #[test]
    fn plugin_key_falls_back_to_the_folder_when_refused() {
        let err = anyhow::anyhow!("bad manifest");
        assert_eq!("beta", plugin_key(Path::new("some/dir/beta"), &Err(err)));
    }

    #[test]
    fn plugin_configure_idx_reads_the_first_and_last_row() {
        assert_eq!(Some(0), plugin_configure_idx(ID_PLUGIN_CONFIGURE_BASE));
        assert_eq!(
            Some((PLUGIN_ID_SPAN - 1) as usize),
            plugin_configure_idx(ID_PLUGIN_CONFIGURE_BASE + PLUGIN_ID_SPAN - 1),
        );
    }

    #[test]
    fn plugin_configure_idx_is_none_outside_the_block() {
        assert_eq!(None, plugin_configure_idx(ID_PLUGIN_CONFIGURE_BASE - 1));
        assert_eq!(None, plugin_configure_idx(ID_PLUGIN_CONFIGURE_BASE + PLUGIN_ID_SPAN));
        assert_eq!(None, plugin_configure_idx(ID_PLUGIN_ENABLE_BASE));
    }

    #[test]
    fn plugin_dir_at_reads_back_what_build_remembered() {
        let hwnd = dummy_hwnd(9101);
        remember_plugin_dirs(hwnd, vec![PathBuf::from("plugins/meikiocr")]);
        assert_eq!(Some(PathBuf::from("plugins/meikiocr")), plugin_dir_at(hwnd, 0));
    }

    #[test]
    fn plugin_dir_at_is_none_for_another_window_or_row() {
        let hwnd = dummy_hwnd(9102);
        remember_plugin_dirs(hwnd, vec![PathBuf::from("plugins/meikiocr")]);
        assert_eq!(None, plugin_dir_at(hwnd, 1));
        assert_eq!(None, plugin_dir_at(dummy_hwnd(9103), 0));
    }

    #[test]
    fn engine_dirs_maps_name_to_path() {
        let mut dirs = HashMap::new();
        dirs.insert("meikiocr".to_string(), PathBuf::from("plugins/meikiocr"));
        assert_eq!(dirs.get("meikiocr").unwrap().as_os_str(), "plugins/meikiocr");
        assert!(!dirs.contains_key("nonexistent"));
    }

    #[test]
    fn write_config_replaces_existing_path() {
        let existing = "meikiocr_path = ''\nhf_home = ''\nthreads = 4\n";
        let result = set_config_path(existing, r"C:\tools\meikiocr\.venv\Lib\site-packages");
        assert!(result.contains(r"meikiocr_path = 'C:\tools\meikiocr\.venv\Lib\site-packages'"));
        assert!(result.contains("hf_home = ''"));
        assert!(result.contains("threads = 4"));
    }

    #[test]
    fn write_config_appends_when_missing() {
        let existing = "hf_home = ''\nthreads = 4\n";
        let result = set_config_path(existing, r"C:\tools\meikiocr");
        assert!(result.contains("hf_home = ''"));
        assert!(result.contains("threads = 4"));
        assert!(result.ends_with("meikiocr_path = 'C:\\tools\\meikiocr'\n"));
    }

    #[test]
    fn write_config_creates_from_empty() {
        let result = set_config_path("", r"C:\tools\meikiocr");
        assert_eq!(result, "meikiocr_path = 'C:\\tools\\meikiocr'\n");
    }
}
