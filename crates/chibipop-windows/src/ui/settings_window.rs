//! The settings window.
//!
//! Modeless - see D9.
//! Numbers are combos, not spins.

use crate::config::{LayoutMode, SentenceMode, FIELD_SOURCES};
use crate::dict::frequency::RankingStrategy;
use crate::library::Role;
use crate::settings::{
    DictRow, SettingsForm, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE,
};
use crate::text::ocr::tag_matches;
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use windows::core::{w, Error, PCWSTR, PWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, EnumFontFamiliesExW, GetDC, GetMonitorInfoW, GetSysColor,
    MonitorFromWindow, PtInRect, ReleaseDC, ScreenToClient, COLOR_BTNFACE, COLOR_WINDOWTEXT,
    ENUMLOGFONTEXW, HFONT, LOGFONTW, MONITORINFO, MONITOR_DEFAULTTONEAREST, SHIFTJIS_CHARSET,
    TEXTMETRICW,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetScrollInfo, INITCOMMONCONTROLSEX, LVCOLUMNW, LVINSERTMARK, LVITEMW,
    LIST_VIEW_ITEM_STATE_FLAGS, NMLISTVIEW, ICC_LISTVIEW_CLASSES, ICC_TAB_CLASSES, LVCF_WIDTH,
    LVIF_TEXT, LVIM_AFTER, LVIR_BOUNDS, LVIS_FOCUSED, LVIS_SELECTED, LVIS_STATEIMAGEMASK,
    LVM_DELETEALLITEMS, LVM_DELETEITEM, LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT, LVM_GETITEMRECT,
    LVM_GETITEMSTATE, LVM_GETITEMTEXTW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETINSERTMARK, LVM_SETINSERTMARKCOLOR,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVNI_SELECTED, LVN_BEGINDRAG, LVN_ITEMCHANGED,
    LVSCW_AUTOSIZE_USEHEADER, LVS_EX_CHECKBOXES, LVS_EX_FULLROWSELECT, LVS_NOCOLUMNHEADER,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, WC_LISTVIEW,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OPENFILENAMEW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, ReleaseCapture, SetCapture, SetFocus,
};
use windows::Win32::UI::Shell::ShellExecuteW;
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
    CssEditor,
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
/// The Terms list.
const ID_TERMS: i32 = 111;
const ID_TERMS_UP: i32 = 112;
const ID_TERMS_DOWN: i32 = 113;
const ID_PASSES: i32 = 114;
const ID_SHOW_SCAN: i32 = 115;
const ID_QUIT: i32 = 116;
const ID_TERMS_ADD: i32 = 117;
const ID_TERMS_REMOVE: i32 = 118;
/// The Frequency list.
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
// 141 was Include / exclude,
// 142 the Not-searched box.
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
/// Engine-log checkbox, OCR tab.
const ID_ENGINE_LOG: i32 = 148;
/// Adapter-log checkbox.
const ID_ADAPTER_LOG: i32 = 149;
/// Include-screenshot checkbox.
const ID_INCLUDE_SCREENSHOT: i32 = 150;
/// Notify-on-add checkbox.
const ID_NOTIFY_ON_ADD: i32 = 151;
/// Customize CSS button.
const ID_CSS_EDITOR: i32 = 152;
/// Sentence combo, Anki tab.
const ID_SENTENCE_MODE: i32 = 156;
/// Static region key button.
const ID_STATIC_REGION_KEY: i32 = 157;
/// "Region hotkey" label.
const ID_STATIC_REGION_LABEL: i32 = 158;
/// Overlay outline checkbox.
const ID_SHOW_STATIC_OVERLAY: i32 = 159;
/// Capture-exclusion hint text.
const ID_STATIC_CAPTURE_HINT: i32 = 160;
/// First-dict-only checkbox.
const ID_FIRST_DICT_ONLY: i32 = 161;
/// OCR clipboard key button.
const ID_OCR_CLIPBOARD_KEY: i32 = 162;
/// Layout-mode combo, General tab.
const ID_LAYOUT_MODE: i32 = 163;
/// Dictionary-styling checkbox.
const ID_DICT_STYLING: i32 = 164;
/// Show-examples checkbox.
const ID_SHOW_EXAMPLES: i32 = 165;
/// Show-attributions checkbox.
const ID_SHOW_ATTRIBUTIONS: i32 = 166;
/// Show-images checkbox.
const ID_SHOW_IMAGES: i32 = 167;
/// Show-part-of-speech checkbox.
const ID_SHOW_POS: i32 = 168;
/// Frequency move-up button.
const ID_FREQ_UP: i32 = 169;
/// Frequency move-down button.
const ID_FREQ_DOWN: i32 = 170;
/// The Pitch list.
const ID_PITCH: i32 = 171;
/// Pitch move-up button.
const ID_PITCH_UP: i32 = 172;
/// Pitch move-down button.
const ID_PITCH_DOWN: i32 = 173;
/// Pitch Add button.
const ID_PITCH_ADD: i32 = 174;
/// Pitch Remove button.
const ID_PITCH_REMOVE: i32 = 175;
/// Ranking-strategy combo, Dictionaries tab.
const ID_RANKING: i32 = 176;

/// First field-map combo id.
const ID_FIELD_MAP_BASE: i32 = 200;

/// Field-map combo choices, in the order they are filled.
///
/// A Win32 combo answers with the index it was filled at, so the fill in
/// `build_field_map_rows` and the read-back in `form` are two halves of
/// one edge and must walk this single sequence. Two lists that merely
/// agree today would map every field to the wrong source the day one of
/// them gains an entry.
///
/// Which sources a mapping may name is a core rule, so the vocabulary is
/// `chibipop::config::FIELD_SOURCES` and this window only renders it. The
/// `"(none)"` sentinel prepended here is not one of them: it is this
/// window's idiom for "this field maps to nothing", dropped by
/// `row_mapping` before save, never a stored value. Prepending it is also
/// why the read-back is offset by one.
const FIELD_MAP_SOURCES: [&str; FIELD_SOURCES.len() + 1] = {
    let mut all = ["(none)"; FIELD_SOURCES.len() + 1];
    let mut i = 0;
    while i < FIELD_SOURCES.len() {
        all[i + 1] = FIELD_SOURCES[i];
        i += 1;
    }
    all
};

/// The sentence-capture combo, in the order it is filled.
///
/// A Win32 combo answers with the index it was filled at, so this table
/// is both halves of the UI edge: the labels going in, and the mode
/// coming back out.
const SENTENCE_MODES: [(SentenceMode, &str); 3] = [
    (SentenceMode::Line, "Current line"),
    (SentenceMode::All, "All lines"),
    (SentenceMode::Static, "Static region"),
];

/// The mode a combo selection names.
///
/// No selection (`-1`, a combo that lost it) reads as the default, which
/// is the item `build` selects when it finds none.
fn sentence_mode_at(selection: isize) -> SentenceMode {
    usize::try_from(selection)
        .ok()
        .and_then(|i| SENTENCE_MODES.get(i))
        .map_or(SentenceMode::Line, |&(mode, _)| mode)
}

/// The layout-mode combo, in the order it is filled.
///
/// The same one-table-per-edge rule as [`SENTENCE_MODES`], and for the
/// same reason: a Win32 combo answers with the index it was filled at,
/// so a second list that merely agreed today would read the wrong mode
/// the day either gained an entry. The Linux window's `LAYOUT_MODES`
/// carries the same two labels.
const LAYOUT_MODES: [(LayoutMode, &str); 2] = [
    (LayoutMode::Roomy, "Roomy \u{2014} one item per line"),
    (LayoutMode::Compact, "Compact \u{2014} one line per dictionary"),
];

/// The layout mode a combo selection names.
///
/// No selection (`-1`, a combo that lost it) reads as the default, which
/// is the item `build` selects when it finds none.
fn layout_mode_at(selection: isize) -> LayoutMode {
    usize::try_from(selection)
        .ok()
        .and_then(|i| LAYOUT_MODES.get(i))
        .map_or(LayoutMode::Roomy, |&(mode, _)| mode)
}

/// The ranking-strategy combo, in the order it is filled.
///
/// The same one-table-per-edge rule as [`SENTENCE_MODES`], and for the
/// same reason: a Win32 combo answers with the index it was filled at, so
/// the labels going in and the strategy coming back out are two halves of
/// this one table. The Linux window's `RANKING_STRATEGIES` carries the
/// same three labels; the kebab-case TOML spellings are
/// [`RankingStrategy`]'s own and never these.
const RANKING_STRATEGIES: [(RankingStrategy, &str); 3] = [
    (RankingStrategy::BestRank, "Best rank \u{2014} the commonest claim wins"),
    (RankingStrategy::Priority, "Priority \u{2014} the highest list that has the word"),
    (RankingStrategy::Median, "Median \u{2014} the middle of what they claim"),
];

/// The strategy a combo selection names.
///
/// No selection (`-1`, a combo that lost it) reads as the default, which
/// is the item `build` selects when it finds none.
fn ranking_strategy_at(selection: isize) -> RankingStrategy {
    usize::try_from(selection)
        .ok()
        .and_then(|i| RANKING_STRATEGIES.get(i))
        .map_or(RankingStrategy::BestRank, |&(strategy, _)| strategy)
}

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
const WHILE_BUSY: [i32; 23] = [
    ID_APPLY,
    ID_QUIT,
    ID_OCR_LANG,
    ID_ENGINE,
    ID_ENGINE_CONFIGURE,
    ID_RANKING,
    ID_TERMS,
    ID_TERMS_UP,
    ID_TERMS_DOWN,
    ID_TERMS_ADD,
    ID_TERMS_REMOVE,
    ID_FREQS,
    ID_FREQ_UP,
    ID_FREQ_DOWN,
    ID_FREQ_ADD,
    ID_FREQ_REMOVE,
    ID_PITCH,
    ID_PITCH_UP,
    ID_PITCH_DOWN,
    ID_PITCH_ADD,
    ID_PITCH_REMOVE,
    ID_ANKI_TEST,
    ID_CHECK_UPDATE,
];

// ---- layout, 96-DPI px ----

const WIN_W: i32 = 560;
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

/// One line above each list.
const DICT_CAP_H: i32 = 18;
/// Beside a four-button column.
const DICT_LIST_H: i32 = 3 * BTN_PITCH + ROW_H;
/// Six 17px rows plus border.
const _: () = assert!((DICT_LIST_H - 2) / 17 >= 6);

/// One section's group height.
///
/// The strategy row is Frequency's alone: it is the rule that reduces
/// *that* list, and drawing it anywhere else would claim it reduces the
/// other two as well (ADR-0014).
fn role_group_h(role: Role) -> i32 {
    let strategy = if role == Role::Frequency { ROW_H + ROW_GAP } else { 0 };
    20 + DICT_CAP_H + strategy + DICT_LIST_H + 8
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

/// One role's section, as the ids it owns.
///
/// The three sections differ only in which controls they own, and a Win32
/// control is reached by its id, so this table *is* the section: `build`
/// creates from it, `WM_NOTIFY` routes a notification back through it, and
/// `move_selected` and `update_list_buttons` act on it. A second list that
/// merely agreed with this one today would act on the wrong section the
/// day either gained a control.
struct Section {
    role: Role,
    /// The ListView.
    list: i32,
    up: i32,
    down: i32,
    add: i32,
    remove: i32,
    /// The group box's own caption.
    group: &'static str,
    /// One line above the list.
    hint: &'static str,
}

/// The three sections, stacked in [`Role::EVERY`] order.
///
/// One list per role, each with its own order and its own checkbox: a
/// mixed archive is a row in every section it has data for, and unticking
/// its definitions may not silently kill its frequency data (ADR-0014).
const SECTIONS: [Section; 3] = [
    Section {
        role: Role::Terms,
        list: ID_TERMS,
        up: ID_TERMS_UP,
        down: ID_TERMS_DOWN,
        add: ID_TERMS_ADD,
        remove: ID_TERMS_REMOVE,
        group: "Terms \u{2014} the definitions a lookup shows",
        hint: "Topmost first, for the selected OCR language. Untick to skip one.",
    },
    Section {
        role: Role::Frequency,
        list: ID_FREQS,
        up: ID_FREQ_UP,
        down: ID_FREQ_DOWN,
        add: ID_FREQ_ADD,
        remove: ID_FREQ_REMOVE,
        group: "Frequency data \u{2014} how common each word is",
        hint: "Topmost first. Untick to leave a list out of the ranking.",
    },
    Section {
        role: Role::Pitch,
        list: ID_PITCH,
        up: ID_PITCH_UP,
        down: ID_PITCH_DOWN,
        add: ID_PITCH_ADD,
        remove: ID_PITCH_REMOVE,
        group: "Pitch accent \u{2014} how each word is said",
        hint: "Topmost first. Untick to hide a dictionary's accents.",
    },
];

/// The section a list id names.
fn section_of_list(id: i32) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.list == id)
}

/// The section a Move button belongs to, and the way it moves.
fn move_button(id: i32) -> Option<(&'static Section, bool)> {
    SECTIONS.iter().find_map(|s| {
        if s.up == id {
            Some((s, true))
        } else if s.down == id {
            Some((s, false))
        } else {
            None
        }
    })
}

/// The section a Remove button belongs to.
fn remove_button(id: i32) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.remove == id)
}

/// Does this id name an Add button?
///
/// One answer for all three, because an import lands in the lists its
/// roles name and never in the one whose button was pressed: the button is
/// per section only so the user need not leave the section to import.
fn is_add_button(id: i32) -> bool {
    SECTIONS.iter().any(|s| s.add == id)
}

/// A click to service.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// The archive's roles pick the lists.
    Add,
    /// Out of every list, whichever section asked.
    Remove(Role),
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

    // Static region key vk.
    static SR_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // OCR clipboard key vk.
    static OCR_CLIP_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Field-map toggle click, by `HWND`.
    static FIELD_MAP_TOGGLE: Cell<Option<isize>> = const { Cell::new(None) };

    // Pending OCR-language switch.
    static LANG_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    // Plugin dirs, by `HWND`.
    static PLUGIN_DIRS: RefCell<Option<(isize, Vec<PathBuf>)>> = const { RefCell::new(None) };

    // The row being dragged, by `HWND`.
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
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
    let escaped = path.replace('\\', "\\\\");
    let new_line = format!("meikiocr_path = \"{escaped}\"");
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

/// Pick a folder via file dialog.
///
/// `None` if cancelled.
unsafe fn pick_folder(owner: HWND, title: &str) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let filter: Vec<u16> = "Any file\0*.*\0\0".encode_utf16().collect();
    let wtitle = wide(title);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: PCWSTR(wtitle.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: same contract as `pick_archives`.
    let picked = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    if !picked {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    path.parent().map(|p| p.to_path_buf())
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
    // SAFETY: `id` is a key-capture button
    // id, a live descendant of `hwnd`;
    // `window_text` / `SetWindowTextW`
    // state their own contracts.
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
            // A role list reports through WM_NOTIFY, so nothing a list
            // itself does arrives here any more. Its buttons still do, and
            // which section each one belongs to is `SECTIONS`' answer
            // rather than a second table of ids that would have to agree.
            if let Some((section, up)) = move_button(id) {
                unsafe { move_selected(hwnd, section, up) };
                return LRESULT(0);
            }
            if let Some(section) = remove_button(id) {
                record_action(hwnd, Action::Remove(section.role));
                return LRESULT(0);
            }
            if is_add_button(id) {
                record_action(hwnd, Action::Add);
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
            if id == ID_SENTENCE_MODE && notify == CBN_SELCHANGE as u16 {
                unsafe { update_static_controls(hwnd) };
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
                ID_ENGINE_CONFIGURE => record_action(hwnd, Action::ConfigureEngine),
                ID_ANKI_TEST => record_click(hwnd, SettingsClick::AnkiTest),
                ID_CHECK_UPDATE => record_click(hwnd, SettingsClick::CheckUpdate),
                ID_CSS_EDITOR => record_click(hwnd, SettingsClick::CssEditor),
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
                ID_STATIC_REGION_KEY => unsafe { begin_capture(hwnd, ID_STATIC_REGION_KEY) },
                ID_OCR_CLIPBOARD_KEY => unsafe { begin_capture(hwnd, ID_OCR_CLIPBOARD_KEY) },
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
            // Both halves of a row arrive as this one notification: the
            // arrows and a click move the selection, and the space bar and
            // a click on the box move the checkbox. Only the selection can
            // stale a Move button, but re-greying off either is one branch
            // instead of two, and a tick needs no bookkeeping of its own -
            // the control is where a row's enabled flag lives until `read`
            // asks for it.
            if nmhdr.code == LVN_ITEMCHANGED
                && section_of_list(nmhdr.id_from as i32).is_some()
            {
                unsafe { update_list_buttons(hwnd) };
            }
            // A drag starts here and is tracked below: the control decides
            // that a press has become a drag and then tracks nothing
            // itself, so from this notification on the gesture is this
            // window's. Which section it belongs to is `SECTIONS`' answer,
            // and it is the *only* list the drop can land in.
            if nmhdr.code == LVN_BEGINDRAG {
                if let Some(section) = section_of_list(nmhdr.id_from as i32) {
                    // SAFETY: LVN_BEGINDRAG's `lparam` is an NMLISTVIEW,
                    // whose first member is the NMHDR just read; the
                    // control guarantees this for the notification it
                    // names.
                    let nm = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                    let origin = (nm.ptAction.x, nm.ptAction.y);
                    unsafe { begin_drag(hwnd, section, nm.iItem, origin) };
                }
            }
            LRESULT(0)
        }
        // The three below only exist while a row is being dragged, so each
        // is claimed only then: with no drag in progress they are the
        // default handler's, unchanged.
        WM_MOUSEMOVE if drag_of(hwnd).is_some() => {
            unsafe { track_drag(hwnd) };
            LRESULT(0)
        }
        WM_LBUTTONUP if drag_of(hwnd).is_some() => {
            unsafe { finish_drag(hwnd) };
            LRESULT(0)
        }
        WM_CAPTURECHANGED if drag_of(hwnd).is_some() => {
            unsafe { cancel_drag(hwnd) };
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

/// The shift that turns a state-image index into item state.
///
/// A ListView has no check field: the checkbox *is* the item's state
/// image, index 1 for clear and 2 for ticked, moved into the high nibble
/// `LVIS_STATEIMAGEMASK` covers. The SDK spells this
/// `INDEXTOSTATEIMAGEMASK`, which is a macro and so has no symbol the
/// `windows` crate could re-export.
const LV_STATE_IMAGE_SHIFT: u32 = 12;

/// The state image a ticked or a clear row carries.
fn check_state(checked: bool) -> u32 {
    let index: u32 = if checked { 2 } else { 1 };
    index << LV_STATE_IMAGE_SHIFT
}

/// Does this item state say ticked?
///
/// Anything else reads as clear, including the 0 carried by a row that
/// predates the extended style: a row with no box drawn on it has not
/// been ticked.
fn state_is_checked(state: u32) -> bool {
    state & LVIS_STATEIMAGEMASK.0 == check_state(true)
}

/// One role's list, empty and ready to fill.
///
/// Report view with one nameless column, because report is the only view
/// `LVS_EX_CHECKBOXES` draws a box in and the row text lives in column 0.
/// The extended style is applied before any row is inserted: comctl32
/// builds the state image list when that style arrives, and a row that
/// predates it gets state image 0 and no box at all.
unsafe fn make_role_list(
    parent: HWND,
    y: i32,
    w: i32,
    id: i32,
    font: Option<HFONT>,
) -> WinResult<HWND> {
    // SAFETY: `parent` is a live pane owned by the caller and `child`
    // states its own contract; every message below goes to the control it
    // just returned, and each struct is fully initialised by `..Default`.
    unsafe {
        let style = LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER;
        let list = child(parent, WC_LISTVIEW, "", WINDOW_STYLE(style) | WS_TABSTOP | WS_BORDER,
            PAD, y, w, DICT_LIST_H, id, font)?;
        let extended = (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as isize;
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(extended as usize)),
            Some(LPARAM(extended)));
        let column = LVCOLUMNW { mask: LVCF_WIDTH, ..Default::default() };
        SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(0)),
            Some(LPARAM(&column as *const _ as isize)));
        // The only column, so this is "take the whole client width" -
        // otherwise a long dictionary name is clipped at zero.
        SendMessageW(list, LVM_SETCOLUMNWIDTH, Some(WPARAM(0)),
            Some(LPARAM(LVSCW_AUTOSIZE_USEHEADER as isize)));
        // The drag's insertion mark is drawn by the control, in whatever
        // colour it was last told; the default is a fixed one and would
        // vanish against a dark row. The rows' own text colour is the one
        // that follows the user's theme by definition.
        SendMessageW(list, LVM_SETINSERTMARKCOLOR, None,
            Some(LPARAM(GetSysColor(COLOR_WINDOWTEXT) as isize)));
        Ok(list)
    }
}

/// One row's own name.
///
/// A ListView has no "how long is this row" message, so the buffer is
/// grown until the control stops filling it. Guessing one size and
/// truncating would be a real bug rather than a cosmetic one: a dictionary
/// is identified by its exact name now (ADR-0014), so a shortened name is
/// a different dictionary.
unsafe fn lv_text(list: HWND, index: i32) -> String {
    // SAFETY: `list` is a live ListView owned by the caller; `item` is
    // fully initialised and its `pszText` points at `buf`, which outlives
    // the call and is described by `cchTextMax` - the contract
    // LVM_GETITEMTEXTW writes against.
    unsafe {
        let mut buf = vec![0u16; 256];
        loop {
            let mut item = LVITEMW {
                iSubItem: 0,
                pszText: PWSTR(buf.as_mut_ptr()),
                cchTextMax: buf.len() as i32,
                ..Default::default()
            };
            let copied = SendMessageW(
                list,
                LVM_GETITEMTEXTW,
                Some(WPARAM(index as usize)),
                Some(LPARAM(&mut item as *mut _ as isize)),
            )
            .0
            .clamp(0, buf.len() as isize) as usize;
            // A full buffer may have been truncated, so it is not an
            // answer; 64Ki wide chars is, because no dictionary title is
            // that long and looping forever is worse than a clipped name.
            if copied + 1 < buf.len() || buf.len() >= 1 << 16 {
                return String::from_utf16_lossy(&buf[..copied]);
            }
            buf = vec![0u16; buf.len() * 2];
        }
    }
}

/// Is this row ticked?
unsafe fn lv_checked(list: HWND, index: i32) -> bool {
    // SAFETY: `list` is a live ListView owned by the caller;
    // LVM_GETITEMSTATE takes the row in `wparam` and the mask in `lparam`
    // and answers with the masked state, carrying no pointer either way.
    unsafe {
        let state = SendMessageW(
            list,
            LVM_GETITEMSTATE,
            Some(WPARAM(index as usize)),
            Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
        )
        .0;
        state_is_checked(state as u32)
    }
}

/// How many rows a list holds.
unsafe fn lv_count(list: HWND) -> i32 {
    // SAFETY: `list` is a live ListView owned by the caller;
    // LVM_GETITEMCOUNT carries no payload.
    unsafe { SendMessageW(list, LVM_GETITEMCOUNT, None, None).0 as i32 }
}

/// The selected row, or -1.
unsafe fn lv_selection(list: HWND) -> i32 {
    // SAFETY: `list` is a live ListView owned by the caller;
    // LVM_GETNEXTITEM takes the row to search after in `wparam` - all ones
    // for "from the top" - and answers with a row index or -1.
    unsafe {
        SendMessageW(
            list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
        .0 as i32
    }
}

/// One row: name and checkbox.
unsafe fn lv_row(list: HWND, index: i32) -> DictRow {
    // SAFETY: `lv_text` and `lv_checked` state their own contracts.
    unsafe { DictRow { name: lv_text(list, index), enabled: lv_checked(list, index) } }
}

/// Every row of a role's list, or `None` if the control is gone.
unsafe fn lv_rows(hwnd: HWND, id: i32) -> Option<Vec<DictRow>> {
    // SAFETY: `id` names a descendant of `hwnd`; a missing one yields
    // `Err` here rather than a dangling handle, and `lv_row` states its
    // own contract.
    unsafe {
        let list = dlg_item(hwnd, id).ok()?;
        Some((0..lv_count(list).max(0)).map(|i| lv_row(list, i)).collect())
    }
}

/// The row holding `name`, if the list has one.
///
/// Compared for equality rather than asked of LVM_FINDITEMW, whose string
/// search is the control's own and not the exact-name rule the config now
/// keys on (ADR-0014).
unsafe fn lv_find(list: HWND, name: &str) -> Option<i32> {
    // SAFETY: `list` is a live ListView owned by the caller; `lv_text`
    // states its own contract.
    unsafe { (0..lv_count(list).max(0)).find(|&i| lv_text(list, i) == name) }
}

/// Overwrite one row in place.
///
/// Text and checkbox both, because a move trades two whole rows: swapping
/// the names alone would leave each dictionary wearing the other's tick.
unsafe fn lv_set(list: HWND, index: i32, row: &DictRow) {
    // SAFETY: `list` is a live ListView owned by the caller; `item` is
    // fully initialised and its `pszText` points at a buffer that outlives
    // the call, which copies the text. `lv_check` states its own contract.
    unsafe {
        let mut text = wide(&row.name);
        let item = LVITEMW {
            iSubItem: 0,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        SendMessageW(list, LVM_SETITEMTEXTW, Some(WPARAM(index as usize)),
            Some(LPARAM(&item as *const _ as isize)));
        lv_check(list, index, row.enabled);
    }
}

/// Tick or clear one row.
///
/// Always after the row exists, never folded into inserting it: with
/// `LVS_EX_CHECKBOXES` comctl32 stamps state image 1 on a new item itself,
/// so an `LVIF_STATE` an insert carried is overwritten and every imported
/// dictionary would arrive unticked.
unsafe fn lv_check(list: HWND, index: i32, checked: bool) {
    // SAFETY: `list` is a live ListView owned by the caller; `item` is
    // fully initialised and carries no pointer of its own.
    unsafe {
        let item = LVITEMW {
            state: LIST_VIEW_ITEM_STATE_FLAGS(check_state(checked)),
            stateMask: LVIS_STATEIMAGEMASK,
            ..Default::default()
        };
        SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(index as usize)),
            Some(LPARAM(&item as *const _ as isize)));
    }
}

/// Append one row; returns its index.
unsafe fn lv_append(list: HWND, row: &DictRow) -> i32 {
    // SAFETY: `list` is a live ListView owned by the caller; `item` is
    // fully initialised and its `pszText` points at a buffer that outlives
    // the call, which copies the text. `lv_check` states its own contract.
    unsafe {
        let mut text = wide(&row.name);
        let item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: lv_count(list),
            iSubItem: 0,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        let at = SendMessageW(list, LVM_INSERTITEMW, None,
            Some(LPARAM(&item as *const _ as isize))).0 as i32;
        if at >= 0 {
            lv_check(list, at, row.enabled);
        }
        at
    }
}

/// Select and scroll to `index`, or clear the selection when it is < 0.
///
/// Every row is cleared first: LVM_SETITEMSTATE with row -1 is the
/// documented way to reach them all at once, and a list holding two
/// selected rows would give the Move buttons two answers.
unsafe fn lv_select(list: HWND, index: i32) {
    // SAFETY: `list` is a live ListView owned by the caller; both structs
    // are fully initialised and neither carries a pointer of its own.
    unsafe {
        let both = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
        let clear = LVITEMW { stateMask: both, ..Default::default() };
        SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(usize::MAX)),
            Some(LPARAM(&clear as *const _ as isize)));
        if index < 0 {
            return;
        }
        let set = LVITEMW { state: both, stateMask: both, ..Default::default() };
        SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(index as usize)),
            Some(LPARAM(&set as *const _ as isize)));
        SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(index as usize)), None);
    }
}

/// Refill, selecting `at` or the last row if the list is shorter.
unsafe fn fill_role_list(list: HWND, rows: &[DictRow], at: i32) {
    // SAFETY: `list` is a live ListView owned by the caller; `lv_append`
    // and `lv_select` state their own contracts.
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for row in rows {
            lv_append(list, row);
        }
        lv_select(list, at.min(rows.len() as i32 - 1));
    }
}

/// Where a move inside one list would land.
///
/// One list per role makes a move a trade with the neighbour and nothing
/// else: there is no second box to cross into, and an empty enabled list
/// is a legitimate "search nothing" rather than a state to defend against
/// (ADR-0014), so no row is pinned in place to keep one non-empty.
fn move_target(len: usize, index: usize, up: bool) -> Option<usize> {
    if index >= len {
        return None;
    }
    if up {
        index.checked_sub(1)
    } else {
        Some(index + 1).filter(|next| *next < len)
    }
}

/// Can a section's Move button act?
///
/// `count` and `selection` are the ListView's own answers, and a selection
/// is negative when the list has none - which is what an emptied list
/// reports, and what greys both Move buttons.
fn can_move(count: i32, selection: i32, up: bool) -> bool {
    match (usize::try_from(count), usize::try_from(selection)) {
        (Ok(len), Ok(index)) => move_target(len, index, up).is_some(),
        _ => false,
    }
}

/// Reorder inside one section.
///
/// The two rows are traded in place rather than the list refilled, and the
/// selection follows the row the user is aiming: a refill empties the list
/// for an instant, `update_list_buttons` would then see nothing selected,
/// and the focus would come off the very Move button being pressed.
unsafe fn move_selected(hwnd: HWND, section: &Section, up: bool) {
    // SAFETY: `section.list` names a live descendant of `hwnd`, created in
    // `build`; a missing one yields `Err` rather than a dangling handle,
    // and each `lv_*` helper states its own contract.
    unsafe {
        let Ok(list) = dlg_item(hwnd, section.list) else { return };
        let cur = lv_selection(list);
        let (Ok(index), Ok(len)) =
            (usize::try_from(cur), usize::try_from(lv_count(list)))
        else {
            return;
        };
        let Some(at) = move_target(len, index, up) else { return };
        let here = lv_row(list, cur);
        let there = lv_row(list, at as i32);
        lv_set(list, cur, &there);
        lv_set(list, at as i32, &here);
        lv_select(list, at as i32);
        update_list_buttons(hwnd);
    }
}

/// Disable what cannot act.
///
/// Focus is moved onto the list a button belongs to *before* that button is
/// disabled. A disabled control keeps the focus Windows gave it and the
/// keyboard then talks to nothing, so pressing Move up until the row
/// reaches the top would strand the user on a dead button - and reaching
/// these buttons by keyboard is the whole point of the tab order.
unsafe fn update_list_buttons(hwnd: HWND) {
    // SAFETY: every id below is a live descendant of `hwnd`, created in
    // `build`, and each `dlg_item` result is checked before use.
    unsafe {
        // Read once: one control holds the focus, so no later button can
        // match a handle this loop has already moved it off.
        let focused = GetFocus();
        for section in &SECTIONS {
            let Ok(list) = dlg_item(hwnd, section.list) else { continue };
            let count = lv_count(list);
            let cur = lv_selection(list);
            for (id, enable) in [
                (section.up, can_move(count, cur, true)),
                (section.down, can_move(count, cur, false)),
                // A row is all Remove needs: an unreadable archive is a
                // row with no roles at all, listed in Terms precisely so
                // it can still be removed (ADR-0014).
                (section.remove, cur >= 0),
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
}

/// A drag in progress: the row, the list it came out of, and where the
/// press landed.
///
/// `origin` is `NMLISTVIEW::ptAction`, the point the button went down at,
/// in that list's own client coordinates - the frame every later reading
/// is taken in, because a drop position is only meaningful against the
/// list the row started in. The section is carried rather than looked up
/// again so a drag cannot end in a list it did not begin in.
#[derive(Clone, Copy)]
struct Drag {
    window: isize,
    section: &'static Section,
    from: i32,
    origin: (i32, i32),
}

/// Shortest travel that turns a press into a reorder, px.
///
/// Every row carries a checkbox, so a press on a row is as likely to be a
/// tick as the start of a drag: without a floor the wobble in a click on
/// that box would flash an insertion line and could land as a one-row
/// move. comctl32 applies its own `SM_CXDRAG` before it sends
/// `LVN_BEGINDRAG` and `drop_gap`'s rounding needs half a row before it
/// answers differently, so this floor is a third guard - and the only one
/// that is this module's own, holds whatever those two do next, and can be
/// asserted with no mouse in the room. `action::selection` sets the same
/// kind of floor on the region overlay's drag.
const DRAG_DEADBAND_PX: i32 = 5;

/// Has the cursor left the point the press landed on?
///
/// Either axis clears it, as `selection::meets_drag_threshold` reads a
/// selection rect. Both axes would refuse a drag straight down the list,
/// which is the gesture itself, and a purely sideways drag needs no
/// refusing: it answers with the row's own position.
fn clears_drag_deadband(origin: (i32, i32), now: (i32, i32)) -> bool {
    (now.0 - origin.0).abs() >= DRAG_DEADBAND_PX
        || (now.1 - origin.1).abs() >= DRAG_DEADBAND_PX
}

/// The gap between rows a cursor sits over, `0..=rows`.
///
/// `top` is row 0's own top in the list's client coordinates, so a
/// scrolled list needs no second question, and the nearest boundary wins
/// because a gap is what the insertion mark is drawn on. The clamp is what
/// confines a drag to its own section: a cursor above or below this list -
/// including one over another role's list - answers with this list's first
/// or last gap and never with another list's row. Each role's order is its
/// own and a row has no meaning in a list it holds no role for (ADR-0014).
fn drop_gap(y: i32, top: i32, row_h: i32, rows: i32) -> i32 {
    if row_h <= 0 || rows <= 0 {
        return 0;
    }
    // Rounded, not truncated: above row 0 the offset goes negative and
    // integer division truncates towards zero, which would read a cursor a
    // row and a half above the list as the gap below its first row.
    let offset = y - top;
    (offset * 2 + row_h).div_euclid(row_h * 2).clamp(0, rows)
}

/// The row a drag from `from` lands on when it is dropped in `gap`.
///
/// The row vacates its own place on the way, so every gap below it loses
/// one: `rows` reads as "last", and every gap above `from` reads as the
/// row it sits over.
fn drop_target(from: i32, gap: i32) -> i32 {
    if gap > from {
        gap - 1
    } else {
        gap
    }
}

/// The insertion mark a gap names: the row, and the side of it.
///
/// The control marks a gap as a row plus a side, so every gap is "before
/// this row" except the one past the end, which is "after the last".
fn insert_mark_at(gap: i32, rows: i32) -> (i32, u32) {
    if gap >= rows {
        (rows - 1, LVIM_AFTER)
    } else {
        (gap, 0)
    }
}

/// Row 0's top and one row's height, in the list's client coordinates.
///
/// Both come off row 0's own bounds, because that top *is* the scroll
/// offset and every row of a report-view ListView is the same height.
/// `None` when the list holds no rows, which is the one case the control
/// refuses to answer - and also the one case no drag can have started in.
unsafe fn lv_row_metrics(list: HWND) -> Option<(i32, i32)> {
    // SAFETY: `list` is a live ListView owned by the caller; `rect` is
    // writable stack storage that outlives the call, and LVM_GETITEMRECT
    // reads the wanted part out of `left` before it overwrites the rect.
    unsafe {
        let mut rect = RECT { left: LVIR_BOUNDS as i32, ..Default::default() };
        let got = SendMessageW(
            list,
            LVM_GETITEMRECT,
            Some(WPARAM(0)),
            Some(LPARAM(&mut rect as *mut _ as isize)),
        );
        if got.0 == 0 {
            return None;
        }
        let row_h = rect.bottom - rect.top;
        (row_h > 0).then_some((rect.top, row_h))
    }
}

/// The cursor, in one control's client coordinates.
///
/// Asked of the mouse rather than decoded out of a message's `lparam`, as
/// `selection::cursor_point` does: a captured drag reports in the
/// capturing window's frame, and the only frame a drop position means
/// anything in is the dragged list's own.
unsafe fn cursor_in(ctrl: HWND) -> (i32, i32) {
    // SAFETY: `pt` is writable stack storage for both calls, and `ctrl` is
    // a live control owned by the caller. A failed `GetCursorPos` leaves
    // `pt` at the origin, which reads as the top of the list.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = ScreenToClient(ctrl, &mut pt);
        (pt.x, pt.y)
    }
}

/// Draw the insertion mark at `at`, or take it away.
unsafe fn lv_insert_mark(list: HWND, at: Option<(i32, u32)>) {
    // SAFETY: `list` is a live ListView owned by the caller; `mark` is
    // fully initialised, declares its own size, and carries no pointer.
    unsafe {
        // Row -1 is the documented "no mark", so an absent gap and a
        // finished drag are the same message.
        let (item, flags) = at.unwrap_or((-1, 0));
        let mark = LVINSERTMARK {
            cbSize: std::mem::size_of::<LVINSERTMARK>() as u32,
            dwFlags: flags,
            iItem: item,
            dwReserved: 0,
        };
        SendMessageW(list, LVM_SETINSERTMARK, None,
            Some(LPARAM(&mark as *const _ as isize)));
    }
}

/// The drag this window has in progress.
fn drag_of(hwnd: HWND) -> Option<Drag> {
    DRAG.with(|c| c.get()).filter(|d| d.window == hwnd.0 as isize)
}

/// Take the row and take the mouse.
///
/// Capture goes on the settings window rather than on the list, mirroring
/// the region overlay (`action::selection`): whoever holds it gets the
/// moves and the button-up, and this window's wndproc is where they are
/// answered and where every other piece of its pending state already
/// lives. comctl32 sends `LVN_BEGINDRAG` and then tracks nothing itself,
/// so the gesture from here on is this module's.
unsafe fn begin_drag(hwnd: HWND, section: &'static Section, from: i32, origin: (i32, i32)) {
    // SAFETY: `hwnd` is the window whose own wndproc is running, so it is
    // live for the duration of this call, which is all `SetCapture` needs.
    unsafe {
        if from < 0 {
            return;
        }
        let window = hwnd.0 as isize;
        DRAG.with(|c| c.set(Some(Drag { window, section, from, origin })));
        SetCapture(hwnd);
    }
}

/// Move the mark to where a drop would land.
unsafe fn track_drag(hwnd: HWND) {
    // SAFETY: `drag.section.list` names a live descendant of `hwnd`,
    // created in `build`; a missing one yields `Err` rather than a
    // dangling handle, and each helper states its own contract.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        let Ok(list) = dlg_item(hwnd, drag.section.list) else { return };
        let now = cursor_in(list);
        let rows = lv_count(list);
        // Below the floor the gesture is still a click, so nothing is
        // marked: a press on a row's checkbox that wobbles must read as a
        // tick, and a mark would promise a move it is not going to make.
        let at = if clears_drag_deadband(drag.origin, now) {
            lv_row_metrics(list)
                .map(|(top, row_h)| insert_mark_at(drop_gap(now.1, top, row_h, rows), rows))
        } else {
            None
        };
        lv_insert_mark(list, at);
    }
}

/// Give the mouse back without reordering.
///
/// Losing the capture - a menu, a task switch, anything that takes the
/// mouse - ends the gesture, so the mark goes and the row stays.
unsafe fn cancel_drag(hwnd: HWND) {
    // SAFETY: as `track_drag`. No `ReleaseCapture` here: this runs because
    // the mouse has already gone somewhere else.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        DRAG.with(|c| c.set(None));
        if let Ok(list) = dlg_item(hwnd, drag.section.list) {
            lv_insert_mark(list, None);
        }
    }
}

/// Commit the drop, or abandon it, and give the mouse back.
///
/// Released outside the window the gesture is abandoned, because leaving
/// the window is the way out of a drag; released outside only the *list*
/// it lands on that list's first or last position, which is `drop_gap`'s
/// clamp and never another role's list.
unsafe fn finish_drag(hwnd: HWND) {
    // SAFETY: as `track_drag`; `ReleaseCapture` has no preconditions and
    // `released_inside` states its own contract.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        // Cleared before the capture goes: `ReleaseCapture` sends this
        // window its own WM_CAPTURECHANGED, and `cancel_drag` would
        // otherwise abandon the very drop this call is committing.
        DRAG.with(|c| c.set(None));
        let _ = ReleaseCapture();
        let Ok(list) = dlg_item(hwnd, drag.section.list) else { return };
        lv_insert_mark(list, None);
        // The cheap veto first: off the window there is nothing to read a
        // drop position for.
        if !released_inside(hwnd) {
            return;
        }
        let now = cursor_in(list);
        if !clears_drag_deadband(drag.origin, now) {
            return;
        }
        let rows = lv_count(list);
        let Some((top, row_h)) = lv_row_metrics(list) else { return };
        let to = drop_target(drag.from, drop_gap(now.1, top, row_h, rows));
        if to == drag.from {
            return;
        }
        // One `move_selected` per row crossed, so a drop and a Move button
        // are the same mutation and there is one implementation of what a
        // move means: trading with the neighbour, repeated, is exactly
        // lifting the row out and putting it back at `to`, and the
        // selection and the buttons follow because that one path already
        // carries them. That same path moves whatever is *selected*, so the
        // dragged row has to become the selection first, and a row the list
        // no longer holds cannot.
        lv_select(list, drag.from);
        if lv_selection(list) != drag.from {
            return;
        }
        for _ in 0..(to - drag.from).abs() {
            move_selected(hwnd, drag.section, to < drag.from);
        }
    }
}

/// Is the cursor still on the window?
///
/// The whole window rect, frame and all: releasing on the title bar is
/// still releasing on the settings window.
unsafe fn released_inside(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is the live settings window; `rect` and `pt` are
    // writable stack storage that outlives every call. A window whose rect
    // cannot be read holds no drop.
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        PtInRect(&rect, pt).as_bool()
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

/// Show/hide static-mode controls.
unsafe fn update_static_controls(hwnd: HWND) {
    // SAFETY: each id is a live descendant
    // of `hwnd`, created in `build`.
    unsafe {
        let is_static = dlg_item(hwnd, ID_SENTENCE_MODE)
            .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
            .is_ok_and(|i| sentence_mode_at(i) == SentenceMode::Static);
        let cmd = if is_static { SW_SHOW } else { SW_HIDE };
        if let Ok(c) = dlg_item(hwnd, ID_STATIC_REGION_LABEL) {
            let _ = ShowWindow(c, cmd);
        }
        if let Ok(c) = dlg_item(hwnd, ID_STATIC_REGION_KEY) {
            let _ = ShowWindow(c, cmd);
        }
        if let Ok(c) = dlg_item(hwnd, ID_SHOW_STATIC_OVERLAY) {
            let _ = ShowWindow(c, cmd);
        }
        if let Ok(c) = dlg_item(hwnd, ID_STATIC_CAPTURE_HINT) {
            let _ = ShowWindow(c, cmd);
        }
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

/// The field map an Apply saves: the rows merged into the saved map, never
/// the rows alone.
///
/// `readings` is one entry per rendered row - field name, and the source its
/// combo currently shows - so its keys are exactly the field names the note
/// type has: the window builds one row per name `modelFieldNames` returned
/// (`app.rs:797-806`) and rebuilds them whenever that list changes
/// (`field_names_match`). A row is therefore a *view* of one mapping, not
/// the mapping itself, and a saved mapping whose field the model lacks - a
/// renamed field, a deleted one, or one belonging to the note type the user
/// just switched away from - has no row to be read out of and must survive
/// the save untouched. Rebuilding from the rows alone silently deleted it
/// the first time the user opened the Anki tab and pressed Apply (ticket
/// 21).
///
/// `"(none)"` is the opposite case and must stay so: a row exists, so the
/// user looked at a field the model *has* and said no. `row_mapping` drops
/// it and the saved value is not handed back.
///
/// Order is the rows' order first - the model's field order, which is the
/// order the user just read down the tab - then the mappings no row covered,
/// keeping the order the config had them in. Rows first makes the saved
/// order equal the displayed order, and the result is a fixed point of this
/// function, so pressing Apply again never reshuffles the user's TOML.
fn merged_field_map(
    saved: &[crate::config::FieldMapping],
    readings: &[(&str, &str)],
) -> Vec<crate::config::FieldMapping> {
    let mut out: Vec<crate::config::FieldMapping> =
        readings.iter().filter_map(|(field, source)| row_mapping(field, source)).collect();
    out.extend(
        saved
            .iter()
            .filter(|m| !readings.iter().any(|(field, _)| *field == m.anki_field))
            .cloned(),
    );
    out
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

/// Discovered text-provider names.
fn discovered_text_providers(
    found: &[(PathBuf, Result<crate::plugin::manifest::Manifest>)],
) -> Vec<String> {
    let mut names = Vec::new();
    for (_, parsed) in found {
        let Ok(m) = parsed else {
            continue;
        };
        if m.roles.contains(&crate::plugin::manifest::Role::TextProvider)
            && !names.contains(&m.name)
        {
            names.push(m.name.clone());
        }
    }
    names
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
    if mode == ApplyMode::Live {
        "Apply"
    } else {
        "Apply && Restart"
    }
}

/// What that button will do.
fn apply_hint(mode: ApplyMode, staged: bool) -> &'static str {
    match (mode, staged) {
        (ApplyMode::Live, false) => "Applying saves your settings and uses them right away.",
        (ApplyMode::Live, true) => {
            "Applying saves your settings and updates your \
             dictionaries in place."
        }
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
    let cell = match id {
        ID_TRIGGER_KEY => &CAPTURED_VK,
        ID_STATIC_REGION_KEY => &SR_CAPTURED_VK,
        ID_OCR_CLIPBOARD_KEY => &OCR_CLIP_CAPTURED_VK,
        _ => &ANKI_CAPTURED_VK,
    };
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

/// Same, for the static key.
fn resolved_sr_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&SR_CAPTURED_VK, hwnd, template)
}

/// Same, for the OCR clipboard key. This is the edge where the window's
/// "Not set" button becomes the form's `None`: nothing inward carries
/// an empty string meaning "off" (ADR-0012).
fn resolved_ocr_clipboard_key(hwnd: HWND, template: Option<&str>) -> Option<String> {
    let key = resolved_captured_key(&OCR_CLIP_CAPTURED_VK, hwnd, template.unwrap_or_default());
    (!key.is_empty()).then_some(key)
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
    /// Engine name → plugin dir.
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
    /// `stale` are names the config's dictionary lists carry that no
    /// installed dictionary answers to (spec D6a); when non-empty a warning
    /// naming them is shown, because that is what a dictionary rename looks
    /// like from in here.
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

    /// The selected theme name.
    pub fn read_theme_name(&self) -> String {
        // SAFETY: `ID_THEME` created in `build`.
        unsafe {
            let idx = dlg_item(self.hwnd, ID_THEME)
                .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                .unwrap_or(0);
            if idx == 1 { "light".into() } else { "dark".into() }
        }
    }

    /// The selected font name.
    pub fn read_font_name(&self) -> String {
        // SAFETY: `ID_FONT` created in `build`.
        unsafe {
            let idx = dlg_item(self.hwnd, ID_FONT)
                .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                .unwrap_or(-1);
            if idx < 0 {
                return String::new();
            }
            self.fonts.get(idx as usize).cloned().unwrap_or_default()
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
                Action::Remove(role) => self.remove_selected(role),
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
        let staged = self.staged.borrow();
        let has_staged = staged.has_staged();
        // SAFETY: `ID_APPLY` and `ID_STATUS` are live children of `self.hwnd`,
        // created in `build`; `SetWindowTextW` copies each string during the
        // call, so the temporaries below outlive every use.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_APPLY) {
                let caption = wide(apply_caption(self.apply_mode));
                let _ = SetWindowTextW(c, PCWSTR(caption.as_ptr()));
            }
            if let Ok(c) = dlg_item(self.hwnd, ID_STATUS) {
                let hint = wide(apply_hint(self.apply_mode, has_staged));
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

    /// Selected engine's plugin dir.
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
        // SAFETY: `ID_TERMS` names a live descendant of `self.hwnd`, made in
        // `build`; `lv_rows` and `fill_role_list` state their own contracts
        // and every handle is checked before it is used.
        unsafe {
            let Some(rows) = lv_rows(self.hwnd, ID_TERMS) else { return };
            staged.terms = rows;
            staged.ocr_language = prev.clone();
            if crate::settings::is_scoped(&staged) {
                if let Some(keys) = crate::settings::scoped_entry(
                    &staged.terms, &staged.unreadable) {
                    staged.per_language.insert(prev, keys);
                }
            }
            let all: Vec<String> = staged.terms.iter().map(|row| row.name.clone()).collect();
            let list = staged.per_language.get(&next).cloned().unwrap_or_default();
            let scoped = scope_rows(&all, &list, &staged.unreadable);
            staged.terms = scoped;
            staged.dict_list_language = next.clone();
            staged.ocr_language = next;
            if let Ok(terms) = dlg_item(self.hwnd, ID_TERMS) {
                fill_role_list(terms, &staged.terms, 0);
            }
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
        // SAFETY: `id` is one of the key capture ids, a live descendant of
        // `self.hwnd`,
        // created in `build`; `SetWindowTextW` copies the
        // string during the call.
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
    ///
    /// One row per field the note type has, so a saved mapping naming a
    /// field it does not have gets no row and the user cannot see it. That
    /// is the deliberate choice: showing a disabled row per missing field
    /// would be more honest, but the rows are two columns of fixed geometry
    /// sized off the model's field count, and an invisible mapping is only
    /// confusing where a deleted one is data loss. `merged_field_map` is
    /// what makes it safe - the save preserves what it never rendered
    /// (ticket 21). Nothing here deletes a mapping the user did not ask to
    /// delete: unmapping is setting a rendered row to `"(none)"`.
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
        // Nothing said about the map seeds nothing: `default_source` already
        // renders an unmapped field as `"(none)"`.
        let existing = self.staged.borrow().field_map.clone().unwrap_or_default();
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

    /// Drop the selected row, out of every section.
    ///
    /// One archive is one library entry, so removing it is not a change to
    /// one list: `stage_remove` drops the name from all three roles
    /// (ADR-0014) and the controls have to say the same thing. `role` only
    /// names the section that asked, and so which selection names the row.
    unsafe fn remove_selected(&self, role: Role) {
        // SAFETY: every `section.list` names a live descendant of
        // `self.hwnd`, created in `build`; a missing one yields `Err` here
        // rather than a dangling handle, and each `lv_*` helper and
        // `update_list_buttons` states its own contract.
        unsafe {
            let Some(asked) = SECTIONS.iter().find(|s| s.role == role) else { return };
            let Ok(list) = dlg_item(self.hwnd, asked.list) else { return };
            let cur = lv_selection(list);
            if cur < 0 {
                return;
            }
            let name = lv_text(list, cur);
            for section in &SECTIONS {
                let Ok(list) = dlg_item(self.hwnd, section.list) else { continue };
                let Some(at) = lv_find(list, &name) else { continue };
                SendMessageW(list, LVM_DELETEITEM, Some(WPARAM(at as usize)), None);
                // The row under the deleted one, or none left to select.
                lv_select(list, at.min(lv_count(list) - 1));
            }
            self.staged.borrow_mut().stage_remove(&name);
            update_list_buttons(self.hwnd);
            self.refresh_apply();
        }
    }

    /// Stage whatever was picked.
    unsafe fn add_picked(&self) {
        // SAFETY: `pick_archives` owns every buffer it hands the dialog;
        // every `section.list` names a live descendant of `self.hwnd`, and
        // `lv_append` and `lv_select` each state their own contract.
        unsafe {
            let picked = pick_archives(self.hwnd);
            for path in picked {
                // The archive's roles pick the lists, and one archive can
                // land in more than one of them.
                let Some(roles) = self.staged.borrow_mut().stage_add(&path) else {
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
                // The bottom of each of its role lists, ticked, leaving
                // every row the user curated where it already is
                // (ADR-0014).
                let row = DictRow { name, enabled: true };
                for section in SECTIONS.iter().filter(|s| roles.has(s.role)) {
                    let Ok(list) = dlg_item(self.hwnd, section.list) else { continue };
                    let at = lv_append(list, &row);
                    // Or an import lands below the fold, unseen.
                    lv_select(list, at);
                }
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
            // Tabs and the role lists need comctl init.
            let icex = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES,
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
            gen.push(group_start("Popup", y, 6 * (ROW_H + ROW_GAP) + 4 * ROW_H + 30)?);
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

            gen.push(child(page, w!("BUTTON"), "Customize CSS\u{2026}",
                WS_TABSTOP,
                FIELD_X, y, FIELD_W, ROW_H, ID_CSS_EDITOR, f)?);
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

            // ---- Entry content ----
            // The render settings' own group rather than four more rows
            // under Popup, and the Linux window groups them the same
            // way: these six decide what an entry *contains*, where the
            // rows above decide how big the panel is. Every one is a
            // portable field, so neither window may drop one.
            y += 12;
            gen.push(group("Entry content", y, ROW_H + ROW_GAP + 5 * ROW_H + 30)?);
            y += 20;
            gen.push(label("Layout", y)?);
            let layout_combo = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_LAYOUT_MODE, f)?;
            gen.push(layout_combo);
            for (i, (mode, text)) in LAYOUT_MODES.iter().enumerate() {
                SendMessageW(layout_combo, CB_ADDSTRING, None,
                    Some(LPARAM(wide(text).as_ptr() as isize)));
                if form.layout_mode == *mode {
                    SendMessageW(layout_combo, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(layout_combo, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(layout_combo, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H + ROW_GAP;
            gen.push(check("Use the dictionary's own fonts and colours", ID_DICT_STYLING,
                  form.dictionary_styling, y)?);
            y += ROW_H;
            gen.push(check("Show example sentences", ID_SHOW_EXAMPLES,
                  form.show_examples, y)?);
            y += ROW_H;
            gen.push(check("Show attributions and footnotes", ID_SHOW_ATTRIBUTIONS,
                  form.show_attributions, y)?);
            y += ROW_H;
            gen.push(check("Show images", ID_SHOW_IMAGES, form.show_images, y)?);
            y += ROW_H;
            gen.push(check("Show part-of-speech labels inside the entry", ID_SHOW_POS,
                  form.show_part_of_speech, y)?);
            y += ROW_H + 18;
            y += 12;
            gen.push(group("Actions", y, ROW_H + 38)?);
            y += 20;
            gen.push(label("OCR clipboard key", y)?);
            let ocr_clipboard_vk =
                crate::config::parse_trigger_key(form.ocr_clipboard_key.as_deref().unwrap_or(""));
            OCR_CLIP_CAPTURED_VK.with(|c| {
                c.set(ocr_clipboard_vk.map(|vk| (h.0 as isize, vk)));
            });
            let ocr_clipboard_name = ocr_clipboard_vk
                .map(crate::config::trigger_key_name)
                .unwrap_or_else(|| "Not set".to_string());
            gen.push(child(
                page,
                w!("BUTTON"),
                &ocr_clipboard_name,
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_OCR_CLIPBOARD_KEY,
                f,
            )?);
            y += ROW_H + 18;
            let y_general = y;

            // ---- Dictionaries ----
            //
            // One section per role, each listing every installed
            // dictionary that holds that role with a checkbox for it, and
            // each ordered on its own. A mixed archive is a row in every
            // section it has data for, because enabled is per role:
            // unticking its definitions may not silently kill its
            // frequency data (ADR-0014).
            y = 0;
            let bx = WIN_W - PAD - BTN_W - 8;
            let list_w = bx - 2 * PAD + 4;
            let hint_w = WIN_W - 2 * PAD - 20;
            for (n, section) in SECTIONS.iter().enumerate() {
                if n > 0 {
                    y += GROUP_GAP;
                }
                // WS_GROUP ends the preceding one, so only the first box
                // may go without it.
                let box_h = role_group_h(section.role);
                dict.push(if n == 0 {
                    group(section.group, y, box_h)?
                } else {
                    group_start(section.group, y, box_h)?
                });
                y += 20;
                dict.push(child(page, w!("STATIC"), section.hint,
                    WINDOW_STYLE(0), PAD, y, hint_w, DICT_CAP_H, 0, f)?);
                y += DICT_CAP_H;
                if section.role == Role::Frequency {
                    // Above this list and no other: the rule reduces the
                    // dictionaries in it and says nothing about the rest.
                    dict.push(child(page, w!("STATIC"), "Combine ranks by",
                        WINDOW_STYLE(0), PAD, y + 4, 110, ROW_H, 0, f)?);
                    let ranking = child(page, w!("COMBOBOX"), "",
                        WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                        PAD + 114, y, list_w - 114, 120, ID_RANKING, f)?;
                    dict.push(ranking);
                    for (at, (strategy, text)) in RANKING_STRATEGIES.iter().enumerate() {
                        SendMessageW(ranking, CB_ADDSTRING, None,
                            Some(LPARAM(wide(text).as_ptr() as isize)));
                        if *strategy == form.ranking_strategy {
                            SendMessageW(ranking, CB_SETCURSEL, Some(WPARAM(at)), None);
                        }
                    }
                    // The default is the item `ranking_strategy_at` reads
                    // a lost selection as, so the two cannot disagree.
                    if SendMessageW(ranking, CB_GETCURSEL, None, None).0 < 0 {
                        SendMessageW(ranking, CB_SETCURSEL, Some(WPARAM(0)), None);
                    }
                    y += ROW_H + ROW_GAP;
                }
                let list = make_role_list(page, y, list_w, section.list, f)?;
                dict.push(list);
                fill_role_list(list, form.list(section.role), 0);
                for (row, (text, id)) in [
                    ("Move up", section.up),
                    ("Move down", section.down),
                    ("Add\u{2026}", section.add),
                    ("Remove", section.remove),
                ]
                .iter()
                .enumerate()
                {
                    dict.push(child(page, w!("BUTTON"), text, WS_TABSTOP,
                          bx, y + row as i32 * BTN_PITCH, BTN_W, ROW_H, *id, f)?);
                }
                y += DICT_LIST_H + 8;
            }
            y += GROUP_GAP;

            // A rebuild is library-only.
            if form.library_empty && !form.terms.is_empty() {
                dict.push(child(page, w!("STATIC"),
                    "chibipop is using a dictionary built outside the app. Adding or \
                     removing here rebuilds from this list only — import your original \
                     .zip files first.",
                    WINDOW_STYLE(0), PAD, y, hint_w, 44, 0, f)?);
                y += 48;
            }

            // Spec D6a: name the entry, because a config name matching
            // nothing installed is also what a renamed archive looks like.
            // Its place is kept rather than dropped, so an unplugged drive
            // does not quietly rewrite the lists (ADR-0014).
            if !stale.is_empty() {
                let msg = format!(
                    "\"{}\" names no installed dictionary — it may have been renamed, or \
                     live on a drive that is not plugged in. Its place is kept; remove \
                     the row if it is gone for good.",
                    stale.join("\", \"")
                );
                dict.push(child(page, w!("STATIC"), &msg, WINDOW_STYLE(0),
                      PAD, y, hint_w, 32, 0, f)?);
                y += 36;
            }
            let y_dict = y;

            // ---- OCR / Debug ----
            y = 0;
            ocr.push(group("OCR / Debug", y, 15 * ROW_H + 38)?);
            y += 20;
            let plugins_root = crate::paths::beside_exe("plugins");
            let found = crate::plugin::discover::discover(&plugins_root);
            let mut engine_names = vec!["builtin".to_string()];
            engine_names.extend(discovered_text_providers(&found));
            // Spec D4: keep it offered.
            if form.engine != "builtin" && !engine_names.contains(&form.engine) {
                engine_names.push(form.engine.clone());
            }
            ocr.push(label("OCR engine", y)?);
            let engine = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W - BTN_W - 8, 220, ID_ENGINE, f)?;
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
                    if m.roles.contains(&crate::plugin::manifest::Role::TextProvider)
                        && !engine_dirs.contains_key(&m.name)
                    {
                        engine_dirs.insert(m.name.clone(), dir.clone());
                    }
                }
            }
            self.engine_dirs = engine_dirs;
            let cfg_btn = child(page, w!("BUTTON"), "Configure…", WS_TABSTOP,
                FIELD_X + FIELD_W - BTN_W, y, BTN_W, ROW_H, ID_ENGINE_CONFIGURE, f)?;
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
            y += ROW_H;
            ocr.push(check("Show which OCR engine is active",
                ID_ENGINE_LOG, form.show_engine_log, y)?);
            y += ROW_H;
            ocr.push(check("Show adapter log in status bar",
                ID_ADAPTER_LOG, form.show_adapter_log, y)?);
            y += ROW_H + 18;
            let y_ocr = y;

            // ---- Anki (own tab) ----
            y = 0;
            ank.push(group("Anki", y, 9 * ROW_H + 34)?);
            y += 20;
            let anki_chk = child(page, w!("BUTTON"), "Enable Anki integration",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD, y, WIN_W - 2 * PAD - 20, ROW_H, ID_ANKI_ENABLED, f)?;
            ank.push(anki_chk);
            SendMessageW(anki_chk, BM_SETCHECK,
                Some(WPARAM(if form.anki_enabled { 1 } else { 0 })), None);
            y += ROW_H;
            ank.push(check("Show notification when a card is added",
                ID_NOTIFY_ON_ADD, form.notify_on_add, y)?);
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
            ank.push(check("Include screenshot when adding",
                ID_INCLUDE_SCREENSHOT, form.include_screenshot, y)?);
            y += ROW_H;
            ank.push(check("First dictionary only",
                ID_FIRST_DICT_ONLY, form.first_dict_only, y)?);
            y += ROW_H;
            ank.push(label("Sentence capture", y)?);
            let sentence_combo = child(page, w!("COMBOBOX"), "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X, y, FIELD_W, 220, ID_SENTENCE_MODE, f)?;
            ank.push(sentence_combo);
            for (i, (mode, text)) in SENTENCE_MODES.iter().enumerate() {
                SendMessageW(sentence_combo, CB_ADDSTRING, None,
                    Some(LPARAM(wide(text).as_ptr() as isize)));
                if form.sentence_mode == *mode {
                    SendMessageW(sentence_combo, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(sentence_combo, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(sentence_combo, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H;
            let is_static = form.sentence_mode == SentenceMode::Static;
            ank.push(child(page, w!("STATIC"), "Region hotkey",
                WINDOW_STYLE(0), PAD, y + 4, LABEL_W, ROW_H,
                ID_STATIC_REGION_LABEL, f)?);
            let sr_vk = crate::config::parse_trigger_key(&form.static_region_key);
            let sr_label = sr_vk
                .map(crate::config::trigger_key_name)
                .unwrap_or_else(|| {
                    if form.static_region_key.is_empty() {
                        "Not set".to_string()
                    } else {
                        form.static_region_key.clone()
                    }
                });
            SR_CAPTURED_VK.with(|c| {
                c.set(sr_vk.map(|vk| (h.0 as isize, vk)));
            });
            ank.push(child(page, w!("BUTTON"), &sr_label, WS_TABSTOP,
                FIELD_X, y, FIELD_W, ROW_H, ID_STATIC_REGION_KEY, f)?);
            y += ROW_H;
            ank.push(check("Show capture region outline",
                ID_SHOW_STATIC_OVERLAY, form.show_static_overlay, y)?);
            y += ROW_H;
            ank.push(child(page, w!("STATIC"),
                "Tip: enable capture exclusion in General for best results",
                WINDOW_STYLE(0), PAD, y, WIN_W - 2 * PAD, ROW_H,
                ID_STATIC_CAPTURE_HINT, f)?);
            if !is_static {
                for &id in &[ID_STATIC_REGION_LABEL, ID_STATIC_REGION_KEY,
                             ID_SHOW_STATIC_OVERLAY, ID_STATIC_CAPTURE_HINT] {
                    if let Ok(c) = dlg_item(h, id) {
                        let _ = ShowWindow(c, SW_HIDE);
                    }
                }
            }
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
            let enabled_plugins = form.enabled_plugins.clone();
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
            //
            // A role's list *is* its control: a row's position is that
            // role's priority and its checkbox is that role's enabled
            // flag, so a read is what the ListView holds. The template
            // answers only for a control that is not there.
            let role_rows = |id: i32, fallback: &[DictRow]| -> Vec<DictRow> {
                lv_rows(h, id).unwrap_or_else(|| fallback.to_vec())
            };
            let terms = role_rows(ID_TERMS, &template.terms);
            let frequency = role_rows(ID_FREQS, &template.frequency);
            let pitch = role_rows(ID_PITCH, &template.pitch);
            let staged = self.staged.borrow();

            let theme = if combo_index(ID_THEME) == 1 { "light" } else { "dark" };
            let sentence_mode = sentence_mode_at(combo_index(ID_SENTENCE_MODE));
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
            let ocr_clipboard_key =
                resolved_ocr_clipboard_key(h, template.ocr_clipboard_key.as_deref());

            // A row is a *view* of one mapping, never the mapping itself, so
            // the save merges; `merged_field_map` owns that decision. The
            // `rows.is_empty()` branch that used to substitute
            // `template.field_map` here is gone, not forgotten: no rows means
            // the window knows no field names, so every saved mapping is
            // unknown and the merge returns that same map untouched.
            let rows = self.field_map_rows.borrow();
            let readings: Vec<(&str, &str)> = rows
                .iter()
                .map(|(name, combo)| {
                    let i = SendMessageW(*combo, CB_GETCURSEL, None, None).0.max(0);
                    let src = FIELD_MAP_SOURCES.get(i as usize).copied().unwrap_or("(none)");
                    (name.as_str(), src)
                })
                .collect();
            // Always an answer, never `None`: a merged map is complete even
            // with no rows, so core's "a window with nothing to say must not
            // wipe the map" rule (ticket 20) has nothing left to protect on
            // this path.
            let saved = template.field_map.as_deref().unwrap_or_default();
            let field_map = Some(merged_field_map(saved, &readings));

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
                layout_mode: layout_mode_at(combo_index(ID_LAYOUT_MODE)),
                dictionary_styling: checked(ID_DICT_STYLING),
                show_examples: checked(ID_SHOW_EXAMPLES),
                show_attributions: checked(ID_SHOW_ATTRIBUTIONS),
                show_images: checked(ID_SHOW_IMAGES),
                show_part_of_speech: checked(ID_SHOW_POS),
                exclude_from_capture: checked(ID_EXCLUDE),
                terms,
                frequency,
                pitch,
                ranking_strategy: ranking_strategy_at(combo_index(ID_RANKING)),
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
                show_engine_log: checked(ID_ENGINE_LOG),
                show_adapter_log: checked(ID_ADAPTER_LOG),
                freq_changed: staged.freq_changed,
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
                notify_on_add: checked(ID_NOTIFY_ON_ADD),
                sentence_mode,
                static_region_key: resolved_sr_key(h, &template.static_region_key),
                show_static_overlay: checked(ID_SHOW_STATIC_OVERLAY),
                ocr_clipboard_key,
                include_screenshot: checked(ID_INCLUDE_SCREENSHOT),
                first_dict_only: checked(ID_FIRST_DICT_ONLY),
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
        SR_CAPTURED_VK.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        OCR_CLIP_CAPTURED_VK.with(|c| {
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
        PLUGIN_DIRS.with(|c| {
            let mut slot = c.borrow_mut();
            if slot.as_ref().is_some_and(|(h, _)| *h == self.hwnd.0 as isize) {
                *slot = None;
            }
        });
        // A window destroyed mid-drag: the OS releases the capture it held,
        // and this drops the row it was carrying so no later window's
        // button-up can find it.
        DRAG.with(|c| {
            if c.get().is_some_and(|d| d.window == self.hwnd.0 as isize) {
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

/// Re-tick and re-order the terms list for one OCR language.
///
/// `list` is that language's own `per_language` entry, so it names the
/// dictionaries it searches in priority order: those rows come first,
/// ticked and in the list's order, and every other installed name follows
/// unticked. `per_language` is term-only (ADR-0014), so this is the Terms
/// section's alone.
fn scope_rows(all: &[String], list: &[String], unreadable: &[String]) -> Vec<DictRow> {
    let readable = |n: &String| !unreadable.iter().any(|u| u == n);
    // A list naming nothing that is installed belongs to some other
    // library, and hiding every dictionary is not what it asked for.
    let named = |n: &String| crate::present::keeps_dict(n, list);
    let row = |name: &String, enabled: bool| DictRow { name: name.clone(), enabled };
    if !all.iter().filter(|n| readable(n)).any(named) {
        return all.iter().map(|n| row(n, true)).collect();
    }
    let keep = |n: &String| !readable(n) || named(n);
    let mut rows: Vec<DictRow> = all.iter().filter(|n| keep(n)).map(|n| row(n, true)).collect();
    rows.sort_by_key(|r| crate::present::list_rank(&r.name, list).unwrap_or(usize::MAX));
    rows.extend(all.iter().filter(|n| !keep(n)).map(|n| row(n, false)));
    rows
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

    /// The combo is core's vocabulary behind this window's sentinel, and
    /// the offset that introduces is what the save read-back decodes by
    /// index. Off by one here maps every field to the wrong source.
    #[test]
    fn field_map_combo_is_the_none_sentinel_then_core_sources() {
        assert_eq!("(none)", FIELD_MAP_SOURCES[0]);
        assert_eq!(&FIELD_SOURCES[..], &FIELD_MAP_SOURCES[1..]);
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

    /// Ticket 21's data loss: the note type no longer has `LegacyAudio`, so
    /// no row renders for it and the pre-merge read-back deleted the
    /// mapping the user never touched.
    #[test]
    fn merged_field_map_keeps_a_mapping_the_model_lacks() {
        let saved = vec![mapping("Front", "expression"), mapping("LegacyAudio", "audio")];
        assert_eq!(
            vec![mapping("Front", "expression"), mapping("LegacyAudio", "audio")],
            merged_field_map(&saved, &[("Front", "expression")]),
        );
    }

    #[test]
    fn merged_field_map_takes_a_rendered_fields_value_from_its_row() {
        let saved = vec![mapping("Front", "expression")];
        assert_eq!(
            vec![mapping("Front", "sentence")],
            merged_field_map(&saved, &[("Front", "sentence")]),
        );
    }

    #[test]
    fn merged_field_map_maps_a_field_the_config_never_named() {
        assert_eq!(
            vec![mapping("Back", "reading")],
            merged_field_map(&[], &[("Back", "reading")]),
        );
    }

    /// A row exists, so the user looked at that field and said no. Merging
    /// must not hand the old value back.
    #[test]
    fn merged_field_map_does_not_resurrect_a_none_row() {
        let saved = vec![mapping("Front", "expression")];
        assert!(merged_field_map(&saved, &[("Front", "(none)")]).is_empty());
    }

    /// The whole subtlety in one assert: same empty combo reading, opposite
    /// outcomes, decided by whether the model has the field at all.
    #[test]
    fn merged_field_map_separates_a_none_row_from_a_field_with_no_row() {
        let saved = vec![mapping("Front", "expression"), mapping("LegacyAudio", "audio")];
        assert_eq!(
            vec![mapping("LegacyAudio", "audio")],
            merged_field_map(&saved, &[("Front", "(none)")]),
        );
    }

    /// Rows in the model's order, then the survivors in the config's.
    #[test]
    fn merged_field_map_orders_rows_first_then_survivors() {
        let saved = vec![
            mapping("OldAudio", "audio"),
            mapping("Front", "sentence"),
            mapping("OldReading", "reading"),
        ];
        assert_eq!(
            vec![
                mapping("Front", "expression"),
                mapping("Back", "glossary"),
                mapping("OldAudio", "audio"),
                mapping("OldReading", "reading"),
            ],
            merged_field_map(&saved, &[("Front", "expression"), ("Back", "glossary")]),
        );
    }

    /// Pressing Apply twice must not reshuffle the user's TOML.
    #[test]
    fn merged_field_map_is_a_fixed_point_under_a_second_apply() {
        let saved = vec![mapping("OldAudio", "audio"), mapping("Front", "sentence")];
        let readings = [("Front", "expression"), ("Back", "glossary")];
        let once = merged_field_map(&saved, &readings);
        assert_eq!(once, merged_field_map(&once, &readings));
    }

    /// AnkiConnect never answered, so no row was ever rendered. This is what
    /// makes the old `rows.is_empty()` branch redundant.
    #[test]
    fn merged_field_map_keeps_everything_when_no_row_was_rendered() {
        let saved = vec![mapping("Front", "expression"), mapping("Back", "glossary")];
        assert_eq!(saved, merged_field_map(&saved, &[]));
    }

    /// The whole seam, not only the decision: a real window renders a note
    /// type that has lost a mapped field, and Apply still saves it.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn reading_a_model_missing_a_mapped_field_keeps_the_mapping() {
        let saved = vec![mapping("Front", "expression"), mapping("LegacyAudio", "audio")];
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.field_map = Some(saved.clone());
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        window.populate_combos(&[], &[], &["Front".to_string()]);
        assert_eq!(Some(saved), window.read(&form).field_map);
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

    #[test]
    fn take_captured_key_routes_the_ocr_clipboard_key_to_its_own_id() {
        let hwnd = HWND(6014 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_OCR_CLIPBOARD_KEY))));

        let got = take_captured_key(hwnd, 0x78);

        assert_eq!(Some((ID_OCR_CLIPBOARD_KEY, "F9".to_string())), got);
        assert_eq!(
            Some((hwnd.0 as isize, 0x78)),
            OCR_CLIP_CAPTURED_VK.with(|c| c.get())
        );
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        OCR_CLIP_CAPTURED_VK.with(|c| c.set(None));
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

    /// The "Not set" button is the form's `None`, never an empty string
    /// standing in for it (ADR-0012).
    #[test]
    fn resolved_ocr_clipboard_key_maps_an_unset_button_to_none() {
        let hwnd = HWND(6015 as *mut core::ffi::c_void);
        OCR_CLIP_CAPTURED_VK.with(|c| c.set(None));

        assert_eq!(None, resolved_ocr_clipboard_key(hwnd, None));
        assert_eq!(None, resolved_ocr_clipboard_key(hwnd, Some("")));
        assert_eq!(Some("f9".to_string()), resolved_ocr_clipboard_key(hwnd, Some("f9")));
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

    // ---- the section table ----

    #[test]
    fn every_role_has_exactly_one_section() {
        assert_eq!(Role::EVERY.len(), SECTIONS.len());
        for role in Role::EVERY {
            assert_eq!(
                1,
                SECTIONS.iter().filter(|s| s.role == role).count(),
                "{role:?} needs exactly one section"
            );
        }
    }

    /// Two sections sharing an id would make `dlg_item` hand back the
    /// wrong control, and one list would answer for two sections.
    #[test]
    fn no_two_dictionary_controls_share_an_id() {
        let mut ids: Vec<i32> = SECTIONS
            .iter()
            .flat_map(|s| [s.list, s.up, s.down, s.add, s.remove])
            .chain([ID_RANKING])
            .collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(total, ids.len(), "every dictionary control needs its own id");
    }

    /// A Move button acts on the section it sits in, never on whichever
    /// list was touched last: that ambiguity is what three independent
    /// lists remove.
    #[test]
    fn each_move_button_names_its_own_section_and_direction() {
        for section in &SECTIONS {
            let (up_owner, up) = move_button(section.up).expect("a Move up button");
            assert_eq!(section.role, up_owner.role);
            assert!(up, "{:?}'s up button must move up", section.role);
            let (down_owner, down) = move_button(section.down).expect("a Move down button");
            assert_eq!(section.role, down_owner.role);
            assert!(!down, "{:?}'s down button must move down", section.role);
        }
        assert!(move_button(ID_APPLY).is_none());
    }

    #[test]
    fn each_remove_button_names_its_own_section() {
        for section in &SECTIONS {
            assert_eq!(Some(section.role), remove_button(section.remove).map(|s| s.role));
        }
        assert!(remove_button(ID_QUIT).is_none());
    }

    /// One Add per section so the user need not leave it, one meaning for
    /// all three: the archive's roles pick the lists.
    #[test]
    fn every_section_has_an_add_button_and_no_other_control_is_one() {
        for section in &SECTIONS {
            assert!(is_add_button(section.add));
            assert!(!is_add_button(section.list));
            assert!(!is_add_button(section.remove));
        }
        assert!(!is_add_button(ID_RANKING));
    }

    #[test]
    fn a_list_id_names_its_own_section() {
        for section in &SECTIONS {
            assert_eq!(Some(section.role), section_of_list(section.list).map(|s| s.role));
        }
        assert!(section_of_list(ID_RANKING).is_none());
    }

    // ---- the checkbox is a state image ----

    /// The two indices comctl32 draws, in the nibble
    /// `LVIS_STATEIMAGEMASK` covers: 1 clear, 2 ticked.
    #[test]
    fn a_ticked_row_carries_state_image_two_and_a_clear_one_carries_one() {
        assert_eq!(0x2000, check_state(true));
        assert_eq!(0x1000, check_state(false));
    }

    #[test]
    fn a_ticked_state_reads_back_ticked_and_a_clear_one_does_not() {
        assert!(state_is_checked(check_state(true)));
        assert!(!state_is_checked(check_state(false)));
    }

    /// Selection and focus share the state word with the checkbox, so a
    /// selected clear row must not read as ticked - the bug that would
    /// silently enable every row the user clicked on.
    #[test]
    fn selection_and_focus_bits_do_not_read_as_a_tick() {
        let live = LVIS_SELECTED.0 | LVIS_FOCUSED.0;
        assert!(!state_is_checked(live));
        assert!(!state_is_checked(check_state(false) | live));
        assert!(state_is_checked(check_state(true) | live));
    }

    /// A row that predates the extended style has no state image at all,
    /// and a row with no box drawn on it has not been ticked.
    #[test]
    fn a_row_with_no_state_image_reads_as_clear() {
        assert!(!state_is_checked(0));
    }

    // ---- moving inside one section ----

    #[test]
    fn up_trades_with_the_row_above() {
        assert_eq!(Some(1), move_target(3, 2, true));
    }

    #[test]
    fn down_trades_with_the_row_below() {
        assert_eq!(Some(1), move_target(3, 0, false));
    }

    #[test]
    fn up_on_the_top_row_refuses() {
        assert_eq!(None, move_target(3, 0, true));
    }

    #[test]
    fn down_on_the_bottom_row_refuses() {
        assert_eq!(None, move_target(3, 2, false));
    }

    /// A selection index the list has outgrown cannot reorder it.
    #[test]
    fn a_move_from_beyond_the_last_row_refuses() {
        assert_eq!(None, move_target(2, 2, true));
        assert_eq!(None, move_target(2, 5, false));
        assert_eq!(None, move_target(0, 0, true));
    }

    /// There is no second box to cross into and no row worth pinning in
    /// place: an empty enabled list is a legitimate "search nothing"
    /// (ADR-0014), so a section's only row simply cannot move.
    #[test]
    fn the_only_row_in_a_section_can_move_neither_way() {
        assert_eq!(None, move_target(1, 0, true));
        assert_eq!(None, move_target(1, 0, false));
    }

    /// Greying is the same question as moving, asked without moving.
    #[test]
    fn the_move_buttons_die_at_that_sections_own_ends() {
        assert!(!can_move(3, 0, true), "the top row cannot go up");
        assert!(can_move(3, 0, false));
        assert!(can_move(3, 2, true));
        assert!(!can_move(3, 2, false), "the bottom row cannot go down");
    }

    #[test]
    fn nothing_selected_greys_both_move_buttons() {
        assert!(!can_move(3, -1, true));
        assert!(!can_move(3, -1, false));
    }

    /// A negative count is what a control that is not there reports.
    #[test]
    fn an_absent_list_greys_both_move_buttons() {
        assert!(!can_move(-1, 0, true));
        assert!(!can_move(-1, 0, false));
    }

    // ---- dragging a row into place ----

    /// The floor is what keeps a click on a row's checkbox a click: press
    /// and release land on one pixel, and no reorder may come out of that.
    #[test]
    fn a_press_and_release_on_one_pixel_is_a_click_and_not_a_drag() {
        assert!(!clears_drag_deadband((40, 30), (40, 30)));
    }

    #[test]
    fn travel_short_of_the_floor_is_still_a_click() {
        let stop = DRAG_DEADBAND_PX - 1;
        assert!(!clears_drag_deadband((40, 30), (40 + stop, 30 + stop)));
        assert!(!clears_drag_deadband((40, 30), (40 - stop, 30 - stop)));
    }

    /// Either axis and either way: a list is dragged up as often as down.
    #[test]
    fn travel_of_the_floor_on_one_axis_becomes_a_drag() {
        assert!(clears_drag_deadband((40, 30), (40, 30 + DRAG_DEADBAND_PX)));
        assert!(clears_drag_deadband((40, 30), (40, 30 - DRAG_DEADBAND_PX)));
        assert!(clears_drag_deadband((40, 30), (40 + DRAG_DEADBAND_PX, 30)));
        assert!(clears_drag_deadband((40, 30), (40 - DRAG_DEADBAND_PX, 30)));
    }

    /// A row is 17px tall (see `DICT_LIST_H`), so a three-row list has its
    /// boundaries at 0, 17, 34 and 51 and each row answers with whichever
    /// of its own two is nearer - the one the mark would be drawn on.
    #[test]
    fn a_cursor_over_a_row_reads_the_nearer_of_its_two_boundaries() {
        assert_eq!(0, drop_gap(0, 0, 17, 3));
        assert_eq!(0, drop_gap(8, 0, 17, 3), "row 0's upper half");
        assert_eq!(1, drop_gap(9, 0, 17, 3), "row 0's lower half");
        assert_eq!(1, drop_gap(17, 0, 17, 3), "the boundary itself");
        assert_eq!(1, drop_gap(25, 0, 17, 3), "row 1's upper half");
        assert_eq!(2, drop_gap(26, 0, 17, 3), "row 1's lower half");
        assert_eq!(3, drop_gap(51, 0, 17, 3), "under the last row");
    }

    /// The clamp *is* the confinement: a cursor dragged out of this list -
    /// over another role's list, or off the window entirely - answers with
    /// this list's own first or last gap, so a row can never cross into a
    /// list it holds no role for (ADR-0014).
    #[test]
    fn a_cursor_outside_the_list_clamps_to_that_lists_own_ends() {
        assert_eq!(0, drop_gap(-9, 0, 17, 3), "a row and a half above it");
        assert_eq!(0, drop_gap(-4000, 0, 17, 3), "far above the window");
        assert_eq!(3, drop_gap(4000, 0, 17, 3), "far below the window");
    }

    /// Row 0's top *is* the scroll offset, so a scrolled list needs no
    /// second one: every gap moves with it and the row under the cursor
    /// stays the row the user is looking at.
    #[test]
    fn a_scrolled_list_reads_its_gaps_from_row_zeros_own_top() {
        assert_eq!(2, drop_gap(0, -34, 17, 6));
        assert_eq!(3, drop_gap(17, -34, 17, 6));
    }

    /// A control with no rows has no gap to drop into, and one that
    /// answered nothing about its row height must not divide by it.
    #[test]
    fn a_list_with_no_rows_or_no_height_reads_the_first_gap() {
        assert_eq!(0, drop_gap(80, 0, 17, 0));
        assert_eq!(0, drop_gap(80, 0, 0, 3));
    }

    /// The dragged row vacates its own place on the way, so the gap just
    /// below it is the place it already holds and every gap further down
    /// loses one.
    #[test]
    fn a_gap_below_the_dragged_row_loses_the_place_that_row_vacates() {
        assert_eq!(0, drop_target(0, 0));
        assert_eq!(0, drop_target(0, 1), "the gap under row 0 is row 0's own");
        assert_eq!(1, drop_target(0, 2));
        assert_eq!(2, drop_target(0, 3));
        assert_eq!(0, drop_target(2, 0));
        assert_eq!(2, drop_target(2, 2));
        assert_eq!(2, drop_target(2, 3));
    }

    /// The mark is a row and a side of it, and only the gap past the last
    /// row is on the far side of one.
    #[test]
    fn the_insertion_mark_sits_above_a_gaps_row_except_past_the_last() {
        assert_eq!((0, 0), insert_mark_at(0, 3));
        assert_eq!((1, 0), insert_mark_at(1, 3));
        assert_eq!((2, 0), insert_mark_at(2, 3));
        assert_eq!((2, LVIM_AFTER), insert_mark_at(3, 3));
    }

    /// The acceptance criterion as arithmetic: a cursor below the list
    /// lands its row last and one above it lands the row first, and both
    /// answers are positions in the list the drag started in.
    #[test]
    fn a_drag_off_either_end_lands_the_row_at_that_end_of_its_own_list() {
        assert_eq!(2, drop_target(0, drop_gap(4000, 0, 17, 3)));
        assert_eq!(0, drop_target(2, drop_gap(-4000, 0, 17, 3)));
    }

    /// One implementation of what a move means: a drop is the Move button
    /// pressed once per row crossed, and that walk has to equal lifting the
    /// row out of the list and putting it back at the drop position. Every
    /// row against every gap, so neither direction nor either end is left
    /// untried.
    #[test]
    fn a_drop_reorders_a_list_exactly_as_repeated_move_buttons_do() {
        let names = ["A", "B", "C", "D"];
        for from in 0..names.len() {
            for gap in 0..=names.len() {
                let to = drop_target(from as i32, gap as i32) as usize;
                let mut walked = names.to_vec();
                let mut at = from;
                while at != to {
                    let next = move_target(walked.len(), at, to < at)
                        .expect("a neighbour to trade with");
                    walked.swap(at, next);
                    at = next;
                }
                let mut lifted = names.to_vec();
                let row = lifted.remove(from);
                lifted.insert(to, row);
                assert_eq!(lifted, walked, "row {from} dropped in gap {gap}");
            }
        }
    }

    /// The centre of one row, in screen coordinates.
    ///
    /// A drag is driven by moving the real cursor, because that is what
    /// `track_drag` and `finish_drag` read: a captured drag reports in the
    /// capturing window's frame, and only the list's own frame means
    /// anything to a drop.
    unsafe fn row_centre(list: HWND, index: i32) -> POINT {
        // SAFETY: `list` is a live ListView owned by the caller; `rect` and
        // `pt` are writable stack storage that outlives every call.
        unsafe {
            let mut rect = RECT { left: LVIR_BOUNDS as i32, ..Default::default() };
            SendMessageW(list, LVM_GETITEMRECT, Some(WPARAM(index as usize)),
                Some(LPARAM(&mut rect as *mut _ as isize)));
            let mut pt = POINT {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
            };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(list, &mut pt);
            pt
        }
    }

    /// The notification the control sends once a press has become a drag,
    /// with the cursor's own point as the one the button went down at.
    unsafe fn send_begin_drag(hwnd: HWND, list: HWND, id: i32, item: i32) {
        // SAFETY: `hwnd` and `list` are live windows owned by the caller,
        // and `nm` is fully initialised stack storage that outlives the
        // send - which is the contract WM_NOTIFY's `lparam` carries.
        unsafe {
            let (x, y) = cursor_in(list);
            let nm = NMLISTVIEW {
                hdr: windows::Win32::UI::Controls::NMHDR {
                    hwndFrom: list,
                    idFrom: id as usize,
                    code: LVN_BEGINDRAG,
                },
                iItem: item,
                ptAction: POINT { x, y },
                ..Default::default()
            };
            SendMessageW(hwnd, WM_NOTIFY, Some(WPARAM(id as usize)),
                Some(LPARAM(&nm as *const _ as isize)));
        }
    }

    /// Move the cursor and tell the window, which is what it sees while it
    /// holds the capture.
    unsafe fn drag_cursor_to(hwnd: HWND, pt: POINT) {
        // SAFETY: `hwnd` is a live window owned by the caller; neither
        // message carries a pointer.
        unsafe {
            let _ = SetCursorPos(pt.x, pt.y);
            SendMessageW(hwnd, WM_MOUSEMOVE, None, None);
        }
    }

    /// Three rows in every section, so a drag has somewhere to go in each.
    fn three_of_each() -> SettingsForm {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Terms A", true), ("Terms B", true), ("Terms C", true)]);
        form.frequency = rows(&[("Freq A", true), ("Freq B", true), ("Freq C", true)]);
        form.pitch = rows(&[("Pitch A", true), ("Pitch B", true), ("Pitch C", true)]);
        form
    }

    /// The gesture end to end on real controls: the notification the
    /// ListView sends starts it, the cursor decides where the row lands,
    /// and the button-up commits through the very path the Move buttons
    /// use - so the selection follows the row that was dragged.
    ///
    /// The insertion mark itself is not asserted here: wine's comctl32
    /// answers 0 to both `LVM_SETINSERTMARK` and `LVM_GETINSERTMARK`, so it
    /// has neither, and this test's whole value would be lost to that gap.
    /// What decides where the mark goes is `drop_gap` and `insert_mark_at`,
    /// pinned above with no control at all.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_row_dragged_onto_the_first_row_becomes_the_first_row() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: `h` is the window just opened, live for this whole test;
        // `ID_TERMS` names the list `build` created inside it, and every
        // helper here states its own contract.
        let (order, selected) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            drag_cursor_to(h, row_centre(list, 2));
            send_begin_drag(h, list, ID_TERMS, 2);
            drag_cursor_to(h, row_centre(list, 0));
            SendMessageW(h, WM_LBUTTONUP, None, None);
            (lv_rows(h, ID_TERMS), lv_selection(list))
        };

        assert_eq!(
            Some(rows(&[("Terms C", true), ("Terms A", true), ("Terms B", true)])),
            order
        );
        assert_eq!(0, selected, "the selection follows the row that was dragged");
    }

    /// Each role's order is its own, so a drag has nowhere to go but its
    /// own list: released over another section it lands on the end of the
    /// list it started in, and that other section does not move a row
    /// (ADR-0014). Both ways, because the sections are stacked and a drag
    /// leaves through the top as easily as the bottom.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drag_over_another_roles_list_clamps_to_its_own_end_and_leaves_that_list_alone() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; all three ids name lists `build` created.
        let (terms, freqs, pitch) = unsafe {
            let terms = dlg_item(h, ID_TERMS).expect("the terms list");
            let freqs = dlg_item(h, ID_FREQS).expect("the frequency list");
            let pitch = dlg_item(h, ID_PITCH).expect("the pitch list");
            // Down and out of Terms, releasing on a Frequency row.
            drag_cursor_to(h, row_centre(terms, 0));
            send_begin_drag(h, terms, ID_TERMS, 0);
            drag_cursor_to(h, row_centre(freqs, 1));
            SendMessageW(h, WM_LBUTTONUP, None, None);
            // Up and out of Pitch, releasing on a Terms row.
            drag_cursor_to(h, row_centre(pitch, 2));
            send_begin_drag(h, pitch, ID_PITCH, 2);
            drag_cursor_to(h, row_centre(terms, 0));
            SendMessageW(h, WM_LBUTTONUP, None, None);
            (lv_rows(h, ID_TERMS), lv_rows(h, ID_FREQS), lv_rows(h, ID_PITCH))
        };

        assert_eq!(
            Some(rows(&[("Terms B", true), ("Terms C", true), ("Terms A", true)])),
            terms,
            "the terms row lands at the terms list's own end, and the second \
             drag released over this list moved nothing in it"
        );
        assert_eq!(
            Some(rows(&[("Pitch C", true), ("Pitch A", true), ("Pitch B", true)])),
            pitch,
            "the pitch row lands at the pitch list's own start"
        );
        assert_eq!(Some(form.frequency.clone()), freqs, "frequency was asked for nothing");
    }

    /// A row carries a checkbox, so a press on a row is as often a tick as
    /// the start of a drag: the tick lands and the row stays where it is.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_click_on_a_rows_checkbox_ticks_it_and_moves_nothing() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; `lv_check` states its own contract.
        let order = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            let on_the_box = row_centre(list, 0);
            lv_check(list, 0, false);
            drag_cursor_to(h, on_the_box);
            // The control read the press as a drag after all; the hand that
            // made it moved two pixels, which is not a reorder.
            send_begin_drag(h, list, ID_TERMS, 0);
            drag_cursor_to(h, POINT { x: on_the_box.x + 1, y: on_the_box.y + 2 });
            SendMessageW(h, WM_LBUTTONUP, None, None);
            lv_rows(h, ID_TERMS)
        };

        assert_eq!(
            Some(rows(&[("Terms A", false), ("Terms B", true), ("Terms C", true)])),
            order
        );
    }

    /// Leaving the window is the way out of a drag: the row stays where it
    /// was, the mouse goes back, and nothing is left holding it.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drag_released_outside_the_window_changes_nothing_and_gives_the_mouse_back() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; `GetWindowRect`, `GetCursorPos` and `PtInRect`
        // all write into or read from stack storage that outlives them.
        let (off_window, order, captured, dragging) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            drag_cursor_to(h, row_centre(list, 0));
            send_begin_drag(h, list, ID_TERMS, 0);
            let mut rect = RECT::default();
            let _ = GetWindowRect(h, &mut rect);
            // Beside the window rather than under it: it is far taller than
            // it is wide, so the room is at the sides.
            let middle = (rect.top + rect.bottom) / 2;
            let beside = if rect.left > 40 { rect.left - 40 } else { rect.right + 40 };
            drag_cursor_to(h, POINT { x: beside, y: middle });
            // The premise, read back rather than assumed: the desktop
            // clamps a cursor to its own bounds.
            let mut landed = POINT::default();
            let _ = GetCursorPos(&mut landed);
            let off = !PtInRect(&rect, landed).as_bool();
            SendMessageW(h, WM_LBUTTONUP, None, None);
            let held = windows::Win32::UI::Input::KeyboardAndMouse::GetCapture();
            (off, lv_rows(h, ID_TERMS), held, drag_of(h).is_some())
        };

        assert!(off_window, "the release has to land off the window to mean anything");
        assert_eq!(Some(form.terms.clone()), order, "an abandoned drag reorders nothing");
        assert_ne!(h, captured, "the capture has to go back");
        assert!(!dragging, "and no row may still be in the air");
    }

    /// Anything may take the mouse mid-drag - a menu, a task switch - and
    /// when it does the gesture is over: a row left in the air would be
    /// dropped by the next stray button-up, wherever the cursor had got to.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_stolen_capture_ends_the_drag_and_leaves_no_row_in_the_air() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; the capture is taken by a live control of this
        // same window and handed straight back.
        let (dragging, order) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            drag_cursor_to(h, row_centre(list, 0));
            send_begin_drag(h, list, ID_TERMS, 0);
            // Far enough that a commit here would really move the row.
            drag_cursor_to(h, row_centre(list, 2));
            SetCapture(list);
            let dragging = drag_of(h).is_some();
            let _ = ReleaseCapture();
            // The button-up that would otherwise have committed the drop.
            SendMessageW(h, WM_LBUTTONUP, None, None);
            (dragging, lv_rows(h, ID_TERMS))
        };

        assert!(!dragging, "the steal has to end the gesture");
        assert_eq!(Some(form.terms.clone()), order, "and no drop may follow it");
    }

    /// The part of the shared path a drag must not route around: a drop
    /// that lands a row at the top grounds Move up, and a disabled control
    /// keeps the focus Windows gave it - so the focus comes off first or
    /// the keyboard is left talking to nothing.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drop_at_the_top_greys_move_up_and_takes_the_focus_off_it() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; `IsWindowEnabled`, `GetFocus` and `SetFocus`
        // read and move the focus between live controls of this window.
        let (parked, order, live, focused) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            let up = dlg_item(h, ID_TERMS_UP).expect("the terms Move up button");
            // Row 1 can go up, so the button is live and worth focusing.
            lv_select(list, 1);
            update_list_buttons(h);
            let _ = SetFocus(Some(up));
            let parked = GetFocus() == up;
            drag_cursor_to(h, row_centre(list, 1));
            send_begin_drag(h, list, ID_TERMS, 1);
            drag_cursor_to(h, row_centre(list, 0));
            SendMessageW(h, WM_LBUTTONUP, None, None);
            let live = windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(up);
            (parked, lv_rows(h, ID_TERMS), live.as_bool(), GetFocus() == list)
        };

        assert!(parked, "the test needs the focus on the button it is about to ground");
        assert_eq!(
            Some(rows(&[("Terms B", true), ("Terms A", true), ("Terms C", true)])),
            order
        );
        assert!(!live, "the top row cannot go up, so Move up has to be dead");
        assert!(focused, "and the focus has to be back on the list");
    }

    // ---- the ranking-strategy combo ----

    /// The table is both halves of the edge, so every label the combo was
    /// filled with reads back as the strategy that put it there.
    #[test]
    fn the_ranking_combo_reads_back_the_strategy_at_each_index() {
        for (at, (strategy, _)) in RANKING_STRATEGIES.iter().enumerate() {
            assert_eq!(*strategy, ranking_strategy_at(at as isize));
        }
    }

    /// `build` selects item 0 when it matches nothing, so a lost selection
    /// has to read as whatever item 0 is.
    #[test]
    fn a_ranking_combo_with_no_selection_reads_the_item_build_would_select() {
        assert_eq!(RANKING_STRATEGIES[0].0, ranking_strategy_at(-1));
        assert_eq!(RANKING_STRATEGIES[0].0, ranking_strategy_at(99));
        assert_eq!(RankingStrategy::default(), ranking_strategy_at(-1));
    }

    /// A strategy the combo cannot offer is one the user cannot pick.
    #[test]
    fn the_ranking_combo_offers_every_strategy_once() {
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            assert_eq!(
                1,
                RANKING_STRATEGIES.iter().filter(|(s, _)| *s == strategy).count(),
                "{strategy:?}"
            );
        }
    }

    // ---- re-scoping the terms list ----

    fn installed_two() -> Vec<String> {
        vec!["Jitendex.org [2026-07-09]".to_string(), "大辞林　第四版".to_string()]
    }

    fn names(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| r.to_string()).collect()
    }

    fn rows(named: &[(&str, bool)]) -> Vec<DictRow> {
        named
            .iter()
            .map(|(name, enabled)| DictRow { name: (*name).to_string(), enabled: *enabled })
            .collect()
    }

    /// No list: every row ticked.
    #[test]
    fn an_empty_language_list_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &[], &[])
        );
    }

    /// Exact names, never a prefix of one.
    #[test]
    fn a_language_list_ticks_and_orders_the_rows_it_names() {
        assert_eq!(
            rows(&[("大辞林　第四版", true), ("Jitendex.org [2026-07-09]", false)]),
            scope_rows(&installed_two(), &names(&["大辞林　第四版"]), &[])
        );
    }

    #[test]
    fn the_list_order_wins_over_the_row_order() {
        let list = names(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        assert_eq!(
            rows(&[("大辞林　第四版", true), ("Jitendex.org [2026-07-09]", true)]),
            scope_rows(&installed_two(), &list, &[])
        );
    }

    /// Stale list: nothing unticked. A name that is only part of an
    /// installed one is stale like any other - the substring match this
    /// replaced is what made a rename look like a deliberate scope.
    #[test]
    fn a_list_matching_nothing_installed_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &names(&["大辞林"]), &[])
        );
    }

    /// Blanks pin nothing.
    #[test]
    fn a_blank_only_list_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &[String::new()], &[])
        );
    }

    /// Unreadable rows cannot scope.
    #[test]
    fn a_list_naming_only_an_unreadable_row_leaves_every_row_ticked() {
        let mut all = installed_two();
        all.push("broken.zip".to_string());
        assert_eq!(
            rows(&[
                ("Jitendex.org [2026-07-09]", true),
                ("大辞林　第四版", true),
                ("broken.zip", true),
            ]),
            scope_rows(&all, &names(&["broken.zip"]), &names(&["broken.zip"]))
        );
    }

    /// It must stay removable, and Terms is the one section an empty role
    /// set is listed in at all (ADR-0014).
    #[test]
    fn an_unreadable_row_survives_a_re_scope_so_it_can_still_be_removed() {
        let mut all = installed_two();
        all.push("broken.zip".to_string());
        assert_eq!(
            rows(&[
                ("大辞林　第四版", true),
                ("broken.zip", true),
                ("Jitendex.org [2026-07-09]", false),
            ]),
            scope_rows(&all, &names(&["大辞林　第四版"]), &names(&["broken.zip"]))
        );
    }

    // ---- the layout budget ----

    /// The list sits beside a four-button column, so it may not be shorter
    /// than one.
    #[test]
    fn a_role_list_is_as_tall_as_its_four_button_column() {
        assert_eq!(3 * BTN_PITCH + ROW_H, DICT_LIST_H);
    }

    /// Only Frequency has a rule to pick, so only its group pays for the
    /// row that picks one.
    #[test]
    fn only_the_frequency_group_is_taller_by_the_strategy_row() {
        let plain = 20 + DICT_CAP_H + DICT_LIST_H + 8;
        assert_eq!(plain, role_group_h(Role::Terms));
        assert_eq!(plain, role_group_h(Role::Pitch));
        assert_eq!(plain + ROW_H + ROW_GAP, role_group_h(Role::Frequency));
    }

    /// The whole seam, not only the decisions: a real window renders three
    /// role sections and every row reads back with the checkbox and the
    /// position it was given, and the strategy combo round-trips.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn three_role_sections_read_back_every_row_with_its_checkbox() {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Terms A", true), ("Terms B", false)]);
        form.frequency = rows(&[("Freq A", true), ("Freq B", true)]);
        form.pitch = rows(&[("Pitch A", false)]);
        form.ranking_strategy = RankingStrategy::Median;
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");

        let back = window.read(&form);

        assert_eq!(form.terms, back.terms);
        assert_eq!(form.frequency, back.frequency);
        assert_eq!(form.pitch, back.pitch);
        assert_eq!(RankingStrategy::Median, back.ranking_strategy);
    }

    /// A checkbox may only affect the section it sits in, and a Move button
    /// only the list beside it: one dictionary supplying two roles has two
    /// rows, and unticking its definitions may not touch its frequency
    /// data (ADR-0014).
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_move_and_a_tick_reach_one_section_only() {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Mixed", true), ("Terms only", true)]);
        form.frequency = rows(&[("Mixed", true), ("Freq only", true)]);
        form.pitch = rows(&[("Pitch only", true)]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: `h` is the window just opened, live for this whole test;
        // both ids name controls `build` created inside it, and each
        // `lv_*` helper states its own contract.
        unsafe {
            let freqs = dlg_item(h, ID_FREQS).expect("the frequency list");
            lv_select(freqs, 0);
            SendMessageW(h, WM_COMMAND, Some(WPARAM(ID_FREQ_DOWN as usize)), None);
            let terms = dlg_item(h, ID_TERMS).expect("the terms list");
            lv_set(terms, 0, &DictRow { name: "Mixed".to_string(), enabled: false });
        }

        let back = window.read(&form);

        assert_eq!(rows(&[("Freq only", true), ("Mixed", true)]), back.frequency);
        assert_eq!(rows(&[("Mixed", false), ("Terms only", true)]), back.terms);
        assert_eq!(form.pitch, back.pitch, "pitch was asked to change nothing");
    }

    /// Every row is listable and removable even with no roles at all: an
    /// unreadable archive is carried in Terms for exactly that reason, so
    /// its Remove button has to be live when its row is selected.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn an_unreadable_row_is_listed_and_its_remove_button_is_live() {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("broken.zip", false)]);
        form.unreadable = names(&["broken.zip"]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above; `IsWindowEnabled` reads a live control.
        let (listed, removable) = unsafe {
            let terms = dlg_item(h, ID_TERMS).expect("the terms list");
            lv_select(terms, 0);
            update_list_buttons(h);
            let remove = dlg_item(h, ID_TERMS_REMOVE).expect("the terms Remove button");
            let live = windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(remove);
            (lv_rows(h, ID_TERMS), live.as_bool())
        };

        assert_eq!(Some(rows(&[("broken.zip", false)])), listed);
        assert!(removable, "an unreadable archive must stay removable");
    }

    /// The window feeds the core seam and the seam decides: a frequency
    /// reorder, tick or strategy change is a reindex, and a terms or pitch
    /// change is a config write and the existing `reload`. The rule itself
    /// is `settings::dictionary_work`'s, so this asserts only that every
    /// control reaches it - a `read` that dropped the strategy on the
    /// floor would silently never rerank.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn only_a_frequency_change_reaches_the_reindex() {
        use crate::settings::DictionaryWork::{None as NoWork, Reindex};
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Mixed", true), ("Terms only", true)]);
        form.frequency = rows(&[("Mixed", true), ("Freq only", true)]);
        form.pitch = rows(&[("Pitch only", true)]);
        let before = crate::settings::apply_to(&form, &crate::config::Config::default());
        // One window per change: they would otherwise accumulate.
        let work = |touch: &dyn Fn(HWND)| {
            let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
                .expect("opening the settings window");
            touch(window.hwnd());
            let after = crate::settings::apply_to(&window.read(&form), &before);
            crate::settings::dictionary_work(&before, &after)
        };
        let reorder = |list: i32, down: i32| {
            move |h: HWND| {
                // SAFETY: both ids name controls `build` created inside
                // `h`, which is live for this call; `lv_select` states its
                // own contract and the button is driven the way a click
                // drives it.
                unsafe {
                    let l = dlg_item(h, list).expect("a role list");
                    lv_select(l, 0);
                    SendMessageW(h, WM_COMMAND, Some(WPARAM(down as usize)), None);
                }
            }
        };
        let untick = |list: i32| {
            move |h: HWND| {
                // SAFETY: as above; `lv_check` states its own contract.
                unsafe {
                    let l = dlg_item(h, list).expect("a role list");
                    lv_check(l, 0, false);
                }
            }
        };

        assert_eq!(NoWork, work(&|_| {}), "an untouched window changes nothing");
        assert_eq!(Reindex, work(&reorder(ID_FREQS, ID_FREQ_DOWN)));
        assert_eq!(Reindex, work(&untick(ID_FREQS)));
        assert_eq!(
            Reindex,
            work(&|h| {
                // SAFETY: `ID_RANKING` names the combo `build` created
                // inside `h`, live for this call.
                unsafe {
                    let combo = dlg_item(h, ID_RANKING).expect("the ranking combo");
                    let at = RANKING_STRATEGIES
                        .iter()
                        .position(|(s, _)| *s == RankingStrategy::Median)
                        .expect("median is offered");
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(at)), None);
                }
            })
        );
        assert_eq!(NoWork, work(&reorder(ID_TERMS, ID_TERMS_DOWN)));
        assert_eq!(NoWork, work(&untick(ID_PITCH)));
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
    fn discovered_text_providers_includes_a_provider() {
        let m = manifest_stub("meikiocr", vec![crate::plugin::manifest::Role::TextProvider]);
        let found = vec![(PathBuf::from("meikiocr"), Ok(m))];
        let names = discovered_text_providers(&found);
        assert_eq!(vec!["meikiocr".to_string()], names);
    }

    #[test]
    fn discovered_text_providers_excludes_a_non_provider_role() {
        let m = manifest_stub("scorer", vec![crate::plugin::manifest::Role::FieldContributor]);
        let found = vec![(PathBuf::from("scorer"), Ok(m))];
        let names = discovered_text_providers(&found);
        assert!(names.is_empty());
    }

    #[test]
    fn discovered_text_providers_excludes_a_refused_manifest() {
        let err = anyhow::anyhow!("plugin \"beta\" declares no roles");
        let found = vec![(PathBuf::from("beta"), Err(err))];
        let names = discovered_text_providers(&found);
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
        let existing = "meikiocr_path = \"\"\nhf_home = ''\nthreads = 4\n";
        let result = set_config_path(existing, r"C:\tools\meikiocr\.venv\Lib\site-packages");
        assert!(result.contains(r#"meikiocr_path = "C:\\tools\\meikiocr\\.venv\\Lib\\site-packages""#));
        assert!(result.contains("hf_home = ''"));
        assert!(result.contains("threads = 4"));
    }

    #[test]
    fn write_config_appends_when_missing() {
        let existing = "hf_home = ''\nthreads = 4\n";
        let result = set_config_path(existing, r"C:\tools\meikiocr");
        assert!(result.contains("hf_home = ''"));
        assert!(result.contains("threads = 4"));
        assert!(result.ends_with("meikiocr_path = \"C:\\\\tools\\\\meikiocr\"\n"));
    }

    #[test]
    fn write_config_creates_from_empty() {
        let result = set_config_path("", r"C:\tools\meikiocr");
        assert_eq!(result, "meikiocr_path = \"C:\\\\tools\\\\meikiocr\"\n");
    }
}
