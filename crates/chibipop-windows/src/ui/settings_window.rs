//! The settings window.
//!
//! The window is modeless. Refer to decision D9.
//! Numeric fields use combo boxes instead of spin controls.

use crate::config::{
    LayoutMode, ScreenshotMode, SelectionButtons, SelectionSeparator, SentenceMode, TripleClick,
    FIELD_SOURCES,
};
use crate::dict::frequency::RankingStrategy;
use crate::library::Role;
use crate::settings::{
    DictRow, SettingsForm, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE,
};
use crate::text::ocr::tag_matches;
use crate::ui::settings_layout::{EntrySpec, SettingId, SettingsLayout, TabId};
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use windows::core::{w, Error, HRESULT, PCWSTR, PWSTR, Result as WinResult};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, DrawTextW, EnumFontFamiliesExW, GetDC, GetMonitorInfoW,
    GetSysColor, MonitorFromWindow, PtInRect, ReleaseDC, ScreenToClient, SelectObject,
    COLOR_BTNFACE, COLOR_WINDOWTEXT, DT_CALCRECT, DT_WORDBREAK, ENUMLOGFONTEXW, HFONT,
    LOGFONTW, MONITORINFO, MONITOR_DEFAULTTONEAREST, SHIFTJIS_CHARSET, TEXTMETRICW,
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
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi,
    SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, ReleaseCapture, SetCapture, SetFocus,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The window reports each user action. `app::run` reads and clears this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    Apply,
    Cancel,
    /// Available only from an active instance.
    Quit,
}

/// The window reports a click event that `app.rs` must handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsClick {
    AnkiTest,
    CheckUpdate,
    CssEditor,
}

/// The mode selects how the window applies changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// `run` applies changes immediately.
    Live,
    /// Saves changes for the next application start.
    Standalone,
}

/// Current Apply state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyState {
    Loaded,
    Pending,
    Applying,
    Applied,
    Failed,
}

impl ApplyState {
    fn label(self) -> &'static str {
        match self {
            Self::Loaded => "Loaded",
            Self::Pending => "Pending",
            Self::Applying => "Applying",
            Self::Applied => "Applied",
            Self::Failed => "Failed",
        }
    }
}

// ---- Control identifiers ----

const ID_APPLY: i32 = 100;
const ID_MODE_LIVE: i32 = 102;
const ID_MODE_HOLD: i32 = 103;
const ID_MODE_TOGGLE: i32 = 153;
const ID_MODE_PRESS: i32 = 154;
const ID_THEME: i32 = 104;
const ID_FONT: i32 = 105;
const ID_MAX_HEIGHT: i32 = 106;
const ID_SUMMARY: i32 = 107;
const ID_HIGHLIGHT: i32 = 108;
const ID_SCROLL: i32 = 109;
const ID_EXCLUDE: i32 = 110;
/// The Terms dictionary list.
const ID_TERMS: i32 = 111;
const ID_TERMS_UP: i32 = 112;
const ID_TERMS_DOWN: i32 = 113;
const ID_PASSES: i32 = 114;
const ID_SHOW_SCAN: i32 = 115;
const ID_QUIT: i32 = 116;
const ID_TERMS_ADD: i32 = 117;
const ID_TERMS_REMOVE: i32 = 118;
/// The Frequency dictionary list.
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
// Identifier 141 was the Include or exclude control.
// Identifier 142 was the Not-searched box.
/// The viewport pane clips the page content.
const ID_VIEWPORT: i32 = 143;
/// The content pane holds the page content.
const ID_CONTENT: i32 = 144;
/// The Updates group box.
const ID_UPDATES: i32 = 145;
/// The OCR engine combo box.
const ID_ENGINE: i32 = 146;
/// OCR Configure button.
const ID_ENGINE_CONFIGURE: i32 = 147;
/// The engine log checkbox.
const ID_ENGINE_LOG: i32 = 148;
/// The Adapter log checkbox.
const ID_ADAPTER_LOG: i32 = 149;
/// The Include screenshot checkbox.
const ID_INCLUDE_SCREENSHOT: i32 = 150;
/// The screenshot capture mode combo box.
const ID_SCREENSHOT_MODE: i32 = 182;
/// The saved screenshot target summary.
const ID_SCREENSHOT_SUMMARY: i32 = 183;
/// The first-use and Alt modifier hint.
const ID_SCREENSHOT_HINT: i32 = 184;
/// The button that clears both saved screenshot targets.
const ID_SCREENSHOT_RESET: i32 = 185;
/// The Notify on add checkbox.
const ID_NOTIFY_ON_ADD: i32 = 151;
/// The Customize CSS button.
const ID_CSS_EDITOR: i32 = 152;
/// The sentence mode combo box.
const ID_SENTENCE_MODE: i32 = 156;
/// The Static region key button.
const ID_STATIC_REGION_KEY: i32 = 157;
/// The Region hotkey label.
const ID_STATIC_REGION_LABEL: i32 = 158;
/// The Overlay outline checkbox.
const ID_SHOW_STATIC_OVERLAY: i32 = 159;
/// The Capture exclusion hint text.
const ID_STATIC_CAPTURE_HINT: i32 = 160;
/// The First dictionary only checkbox.
const ID_FIRST_DICT_ONLY: i32 = 161;
/// The OCR clipboard key button.
const ID_OCR_CLIPBOARD_KEY: i32 = 162;
/// The popup layout combo box.
const ID_LAYOUT_MODE: i32 = 163;
/// The Dictionary styling checkbox.
const ID_DICT_STYLING: i32 = 164;
/// The Show examples checkbox.
const ID_SHOW_EXAMPLES: i32 = 165;
/// The Show attributions checkbox.
const ID_SHOW_ATTRIBUTIONS: i32 = 166;
/// The Show images checkbox.
const ID_SHOW_IMAGES: i32 = 167;
/// The Show part of speech checkbox.
const ID_SHOW_POS: i32 = 168;
/// The Move up button for the Frequency list.
const ID_FREQ_UP: i32 = 169;
/// The Move down button for the Frequency list.
const ID_FREQ_DOWN: i32 = 170;
/// The Pitch dictionary list.
const ID_PITCH: i32 = 171;
/// The Move up button for the Pitch list.
const ID_PITCH_UP: i32 = 172;
/// The Move down button for the Pitch list.
const ID_PITCH_DOWN: i32 = 173;
/// The Add button for the Pitch list.
const ID_PITCH_ADD: i32 = 174;
/// The Remove button for the Pitch list.
const ID_PITCH_REMOVE: i32 = 175;
/// The Ranking strategy combo box on the Dictionaries tab.
const ID_RANKING: i32 = 176;
/// This identifier names the checkbox for `edge_autoscroll`.
const ID_EDGE_AUTOSCROLL: i32 = 177;
/// This identifier names the combo box for `selection_buttons`.
const ID_SELECTION_BUTTONS: i32 = 178;
/// This identifier names the combo box for `selection_separator`.
const ID_SELECTION_SEPARATOR: i32 = 179;
/// This identifier names the combo box for `triple_click`.
const ID_TRIPLE_CLICK: i32 = 180;
/// This identifier names the checkbox for `include_dictionary_name`.
const ID_INCLUDE_DICTIONARY_NAME: i32 = 181;
/// Furigana filter checkbox.
const ID_DISCARD_FURIGANA: i32 = 186;
const ID_SCREENSHOT_HOTKEY: i32 = 187;
const ID_SCREENSHOT_KEY_CLEAR: i32 = 188;
const ID_ANKI_ADD_KEY_CLEAR: i32 = 189;
const ID_STATIC_REGION_KEY_CLEAR: i32 = 190;
const ID_OCR_CLIPBOARD_KEY_CLEAR: i32 = 191;
const ID_SHOW_LIVE_LOGS: i32 = 192;
const ID_APPLY_STATE: i32 = 193;
const ID_RUNTIME_STATUS: i32 = 194;
const ID_SEARCH_KEY: i32 = 195;
const ID_SENTENCE_SEARCH_KEY: i32 = 196;
const ID_SEARCH_KEY_CLEAR: i32 = 197;
const ID_SENTENCE_SEARCH_KEY_CLEAR: i32 = 198;
const ID_OPEN_DICTIONARY_SEARCH: i32 = 199;
const ID_OPEN_SENTENCE_SEARCH: i32 = 91;
const ID_OCR_SENTENCE_SEARCH: i32 = 92;
const ID_SUB_POPUPS: i32 = 93;


/// The first field-map combo identifier.
const ID_FIELD_MAP_BASE: i32 = 200;

/// Choices for the field-map combo boxes in fill order.
///
/// A Win32 combo box returns a selection index. `build_field_map_row` adds
/// rows, and `read` reads them. These operations form one interface.
/// Both operations must process this exact sequence. If the lists diverge,
/// a field can use an incorrect source.
///
/// Core rules specify which sources a field map can name. The source list is
/// `chibipop::config::FIELD_SOURCES`. The window displays only that list.
/// The window adds the `"(none)"` sentinel before the source list.
/// `row_mapping` removes this sentinel before save. The system never stores
/// this sentinel. The extra entry shifts the read index by one.
const FIELD_MAP_SOURCES: [&str; FIELD_SOURCES.len() + 1] = {
    let mut all = ["(none)"; FIELD_SOURCES.len() + 1];
    let mut i = 0;
    while i < FIELD_SOURCES.len() {
        all[i + 1] = FIELD_SOURCES[i];
        i += 1;
    }
    all
};

/// The pump adds this many field rows per cycle.
const FIELD_MAP_ROWS_PER_PUMP: usize = 4;

struct PendingFieldMap {
    fields: Vec<String>,
    existing: Vec<crate::config::FieldMapping>,
    next: usize,
}

impl PendingFieldMap {
    fn new(fields: Vec<String>, existing: Vec<crate::config::FieldMapping>) -> Self {
        Self {
            fields,
            existing,
            next: 0,
        }
    }
}

struct BuiltEntry {
    id: SettingId,
    label: String,
    controls: Vec<HWND>,
    top: i32,
    height: i32,
}

#[derive(Clone, Copy)]
enum HorizontalLayout {
    Fixed,
    Stretch,
    MoveRight,
    Quarter(u8),
}

struct ControlRuntime {
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    dropdown_height: Option<i32>,
    horizontal: HorizontalLayout,
    wraps: bool,
}

struct EntryRuntime {
    id: SettingId,
    label: String,
    controls: Vec<ControlRuntime>,
    top: Cell<i32>,
    base_height: Cell<i32>,
    initial_height: i32,
}

struct SectionRuntime {
    frame: HWND,
    top: Cell<i32>,
    height: Cell<i32>,
    entries: Vec<EntryRuntime>,
}

struct TabRuntime {
    id: TabId,
    label: String,
    sections: Vec<SectionRuntime>,
    page_height: Cell<i32>,
}

#[derive(Clone, Copy, Default)]
struct ConditionalTabs {
    engine: Option<u32>,
    static_key: Option<u32>,
    static_overlay: Option<u32>,
}

/// The sentence capture combo box in fill order.
///
/// A Win32 combo box returns a selection index. The table defines the labels
/// and output modes. The first item supplies the default.
const SENTENCE_MODES: [(SentenceMode, &str); 4] = [
    (SentenceMode::Sentence, "Full sentence"),
    (SentenceMode::Line, "Current line"),
    (SentenceMode::All, "All lines"),
    (SentenceMode::Static, "Static region"),
];

/// The mode for a combo box selection.
///
/// An empty selection (`-1`) uses the default item. `build` selects this
/// item when no match exists.
fn sentence_mode_at(selection: isize) -> SentenceMode {
    usize::try_from(selection)
        .ok()
        .and_then(|i| SENTENCE_MODES.get(i))
        .map_or(SentenceMode::Sentence, |&(mode, _)| mode)
}

/// Returns the screenshot mode for a combo-box selection.
fn screenshot_mode_at(selection: isize) -> ScreenshotMode {
    usize::try_from(selection)
        .ok()
        .and_then(|i| ScreenshotMode::ALL.get(i).copied())
        .unwrap_or_default()
}

/// Formats the saved fixed targets for the Anki settings page.
fn screenshot_target_summary(form: &SettingsForm) -> String {
    screenshot_target_summary_values(
        form.cfg.actions.screenshot.fixed_region,
        form.cfg.actions.screenshot.fixed_window.as_ref(),
    )
}

/// Formats saved fixed targets from their current configuration values.
fn screenshot_target_summary_values(
    region: Option<[i32; 4]>,
    window: Option<&crate::config::ScreenshotWindow>,
) -> String {
    match (region, window) {
        (None, None) => "No saved screenshot targets.".into(),
        (Some([x, y, w, h]), None) => format!("Saved region: ({x}, {y}, {w}x{h})"),
        (None, Some(window)) => {
            format!("Saved window: class {:?}, title {:?}", window.app_id, window.title)
        }
        (Some([x, y, w, h]), Some(window)) => format!(
            "Saved region: ({x}, {y}, {w}x{h}) | window: class {:?}, title {:?}",
            window.app_id, window.title
        ),
    }
}

/// The layout mode combo box in fill order.
///
/// The table obeys the single-table rule of [`SENTENCE_MODES`]. A Win32
/// combo box returns a selection index. A separate list can diverge when
/// either list gains an entry. That mismatch returns the wrong mode.
/// The Linux window definition `LAYOUT_MODES` contains the same two labels.
const LAYOUT_MODES: [(LayoutMode, &str); 2] = [
    (LayoutMode::Roomy, "Roomy \u{2014} one item per line"),
    (LayoutMode::Compact, "Compact \u{2014} one line per dictionary"),
];

/// The layout mode for a combo box selection.
///
/// An empty selection (`-1`) uses the default item. `build` selects this
/// item when no match exists.
fn layout_mode_at(selection: isize) -> LayoutMode {
    usize::try_from(selection)
        .ok()
        .and_then(|i| LAYOUT_MODES.get(i))
        .map_or(LayoutMode::Roomy, |&(mode, _)| mode)
}

/// The table lists selection button modes in combo box order.
const SELECTION_BUTTONS: [(SelectionButtons, &str); 2] = [
    (SelectionButtons::PrimaryAdditive, "Primary additive"),
    (SelectionButtons::PrimaryReplacing, "Primary replacing"),
];

/// The table lists selection separators in combo box order.
const SELECTION_SEPARATORS: [(SelectionSeparator, &str); 4] = [
    (SelectionSeparator::Ellipsis, "Ellipsis (…)"),
    (SelectionSeparator::Space, "Space"),
    (SelectionSeparator::LineBreak, "Line break"),
    (SelectionSeparator::ListItems, "List items"),
];

/// The table lists triple-click modes in combo box order.
const TRIPLE_CLICKS: [(TripleClick, &str); 3] = [
    (TripleClick::Sense, "Sense"),
    (TripleClick::SenseWithExamples, "Sense with examples"),
    (TripleClick::Line, "Line"),
];

fn selection_buttons_at(selection: isize) -> SelectionButtons {
    usize::try_from(selection)
        .ok()
        .and_then(|i| SELECTION_BUTTONS.get(i))
        .map_or(SelectionButtons::PrimaryAdditive, |&(value, _)| value)
}

fn selection_separator_at(selection: isize) -> SelectionSeparator {
    usize::try_from(selection)
        .ok()
        .and_then(|i| SELECTION_SEPARATORS.get(i))
        .map_or(SelectionSeparator::Ellipsis, |&(value, _)| value)
}

fn triple_click_at(selection: isize) -> TripleClick {
    usize::try_from(selection)
        .ok()
        .and_then(|i| TRIPLE_CLICKS.get(i))
        .map_or(TripleClick::SenseWithExamples, |&(value, _)| value)
}


/// The ranking strategy combo box in fill order.
///
/// The table obeys the single-table rule of [`SENTENCE_MODES`]. A Win32
/// combo box returns a selection index. Input labels and output strategies
/// form one table. The Linux window definition `RANKING_STRATEGIES` contains
/// the same three labels. The kebab-case TOML values belong to
/// [`RankingStrategy`], not to this table.
const RANKING_STRATEGIES: [(RankingStrategy, &str); 3] = [
    (RankingStrategy::BestRank, "Best rank \u{2014} the commonest claim wins"),
    (RankingStrategy::Priority, "Priority \u{2014} the highest list that has the word"),
    (RankingStrategy::Median, "Median \u{2014} the middle of what they claim"),
];

/// The ranking strategy for a combo box selection.
///
/// An empty selection (`-1`) uses the default item. `build` selects this
/// item when no match exists.
fn ranking_strategy_at(selection: isize) -> RankingStrategy {
    usize::try_from(selection)
        .ok()
        .and_then(|i| RANKING_STRATEGIES.get(i))
        .map_or(RankingStrategy::BestRank, |&(strategy, _)| strategy)
}

/// The first plugin enable identifier.
const ID_PLUGIN_ENABLE_BASE: i32 = 1000;
/// The first plugin configure identifier.
const ID_PLUGIN_CONFIGURE_BASE: i32 = 1500;
/// The plugin identifier block size.
const PLUGIN_ID_SPAN: i32 = 100;

// Win32 messages for the tab control.
const TCM_FIRST: u32 = 0x1300;
const TCM_GETCURSEL_MSG: u32 = TCM_FIRST + 11;
const TCM_SETCURSEL_MSG: u32 = TCM_FIRST + 12;
const TCM_INSERTITEMW_MSG: u32 = TCM_FIRST + 62;
const TCIF_TEXT_VAL: u32 = 0x0001;
// TCN_SELCHANGE = -551 as u32.
const TCN_SELCHANGE_CODE: u32 = (-551i32) as u32;
const TAB_H: i32 = 28;

/// The Win32 NMHDR memory layout.
#[repr(C)]
struct NmhdrRaw {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

/// The Win32 TCITEMW memory layout.
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

/// The list contains controls that the Apply action disables.
const WHILE_BUSY: [i32; 25] = [
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
    ID_SCREENSHOT_MODE,
    ID_SCREENSHOT_RESET,
];

// ---- Layout dimensions in 96-DPI pixels ----

const WIN_W: i32 = 560;
const MIN_CLIENT_W: i32 = 520;
const MIN_CLIENT_H: i32 = 430;
const PAD: i32 = 14;
const ROW_H: i32 = 24;
const ROW_GAP: i32 = 6;
const GROUP_GAP: i32 = 10;
const BTN_W: i32 = 120;
const BTN_PITCH: i32 = ROW_H + 4;
const LABEL_W: i32 = 178;
const FIELD_X: i32 = PAD + LABEL_W;
const FIELD_W: i32 = WIN_W - FIELD_X - PAD - 16;
const STATUS_H: i32 = 44;
/// The first vertical coordinate below the tab strip.
const CONTENT_Y: i32 = PAD + TAB_H + 4;
/// The vertical offset below the top of the bottom row.
const BOTTOM_UPDATE_DY: i32 = 20;
const BOTTOM_APPLY_STATE_DY: i32 = BOTTOM_UPDATE_DY + ROW_H + 8 + GROUP_GAP;
const BOTTOM_RUNTIME_DY: i32 = BOTTOM_APPLY_STATE_DY + ROW_H;
const BOTTOM_STATUS_DY: i32 = BOTTOM_RUNTIME_DY + ROW_H;
const BOTTOM_BTN_DY: i32 = BOTTOM_STATUS_DY + STATUS_H + 8;
/// The height of the bottom row.
const BOTTOM_H: i32 = BOTTOM_BTN_DY + ROW_H + 8;
/// Each bottom-row item stores a control identifier, a horizontal coordinate,
/// and a vertical offset.
const BOTTOM_ROW: [(i32, i32, i32); 7] = [
    (ID_UPDATES, PAD - 6, 0),
    (ID_CHECK_UPDATE, PAD, BOTTOM_UPDATE_DY),
    (ID_APPLY_STATE, PAD, BOTTOM_APPLY_STATE_DY),
    (ID_RUNTIME_STATUS, PAD, BOTTOM_RUNTIME_DY),
    (ID_STATUS, PAD, BOTTOM_STATUS_DY),
    (ID_APPLY, 0, BOTTOM_BTN_DY),
    (ID_QUIT, PAD, BOTTOM_BTN_DY),
];
/// The height of one scroll line at 96 DPI.
const SCROLL_LINE: i32 = 20;
/// The number of scroll lines per wheel notch.
const WHEEL_LINES: i32 = 3;

// ---- Dictionaries tab ----

/// The height of one text line above each list.
#[cfg(test)]
const DICT_CAP_H: i32 = 18;
/// The dictionary list height matches a column of four buttons.
const DICT_LIST_H: i32 = 3 * BTN_PITCH + ROW_H;
/// The list has space for six 17-pixel rows and a border.
const _: () = assert!((DICT_LIST_H - 2) / 17 >= 6);

/// The group height for one section.
///
/// The strategy row belongs to the Frequency list only. It gives the rule
/// for that list. If the row appeared elsewhere, users can think that it
/// also reduces the other two lists. Refer to ARCHITECTURE.md#dictionary-and-lookup.
#[cfg(test)]
fn role_group_h(role: Role) -> i32 {
    let strategy = if role == Role::Frequency { ROW_H + ROW_GAP } else { 0 };
    20 + DICT_CAP_H + strategy + DICT_LIST_H + 8
}

// ---- Field-map columns ----

const COL_GAP: i32 = 12;
const COL_AREA_W: i32 = WIN_W - 2 * PAD - 20;
const COL_W: i32 = (COL_AREA_W - COL_GAP) / 2;
const COL_LABEL_W: i32 = 120;
const COL_LABEL_GAP: i32 = 4;
const COL_COMBO_W: i32 = COL_W - COL_LABEL_W - COL_LABEL_GAP;
const COL_DROPPED_W: i32 = 150;
const COL_LABEL_MAX_CHARS: usize = 18;

// ---- Plugins ----

/// The status area height allows a long refusal reason.
const PLUGIN_STATUS_H: i32 = ROW_H + 16;
/// The height of one plugin row.
const PLUGIN_ROW_H: i32 = 2 * ROW_H + PLUGIN_STATUS_H;

/// A Section owns one role and its control identifiers.
///
/// The three sections differ only in their control identifiers.
/// Win32 identifies controls by their identifiers. The table defines each section.
/// `build` creates controls from this table. `WM_NOTIFY` routes notifications
/// through this table. `move_selected` and `update_list_buttons` use this table.
/// A second list can diverge and select the wrong section.
struct Section {
    role: Role,
    /// The ListView control.
    list: i32,
    up: i32,
    down: i32,
    add: i32,
    remove: i32,
}

/// The three sections use [`Role::EVERY`] order.
///
/// Each role has one list with its own order and checkbox. A mixed archive
/// appears in each section that provides its data. The window must not disable
/// its frequency data when the user clears its definitions
/// (ARCHITECTURE.md#dictionary-and-lookup).
const SECTIONS: [Section; 3] = [
    Section {
        role: Role::Terms,
        list: ID_TERMS,
        up: ID_TERMS_UP,
        down: ID_TERMS_DOWN,
        add: ID_TERMS_ADD,
        remove: ID_TERMS_REMOVE,
    },
    Section {
        role: Role::Frequency,
        list: ID_FREQS,
        up: ID_FREQ_UP,
        down: ID_FREQ_DOWN,
        add: ID_FREQ_ADD,
        remove: ID_FREQ_REMOVE,
    },
    Section {
        role: Role::Pitch,
        list: ID_PITCH,
        up: ID_PITCH_UP,
        down: ID_PITCH_DOWN,
        add: ID_PITCH_ADD,
        remove: ID_PITCH_REMOVE,
    },
];

/// Returns the Section for a list identifier.
fn section_of_list(id: i32) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.list == id)
}

/// Returns the Section for a Move button and its direction.
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

/// Returns the Section for a Remove button.
fn remove_button(id: i32) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.remove == id)
}

/// Checks whether an identifier names an Add button.
///
/// One handler returns one result for all three buttons. Imported data
/// populates lists by role name, not by clicked button. Each section has a
/// button, so users can import data from that section.
fn is_add_button(id: i32) -> bool {
    SECTIONS.iter().any(|s| s.add == id)
}

/// Lists click events to process.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Archive roles select the lists that receive the data.
    Add,
    /// The handler removes the item from all lists, regardless of its section.
    Remove(Role),
    ConfigureEngine,
    ResetScreenshotTargets,
}

fn class_name() -> PCWSTR {
    w!("ChibipopSettingsClass")
}

/// Returns the viewport and content pane handles.
fn pane_class_name() -> PCWSTR {
    w!("ChibipopSettingsPaneClass")
}

/// Scales a 96-DPI value for the current display DPI.
///
/// The application uses PER_MONITOR_AWARE_V2 mode.
fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    let dpi = window_dpi(hwnd);
    (v as i64 * dpi as i64 / 96) as i32
}

fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: Invalid handles yield null roots and zero DPI.
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        WINDOW_DPI.with(|slot| slot.get())
            .filter(|(owner, _)| *owner == root.0 as isize)
            .map(|(_, dpi)| dpi)
            .unwrap_or_else(|| GetDpiForWindow(hwnd).max(96))
    }
}

/// Gets the monitor work-area height.
///
/// The function measures physical pixels. It returns None when the height is
/// unknown.
fn work_area_height(hwnd: HWND) -> Option<i32> {
    // SAFETY: `hwnd` can be invalid because MonitorFromWindow selects
    // the nearest monitor. `mi` sets `cbSize` to its structure size,
    // as GetMonitorInfoW requires.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(hmon, &mut mi)
            .as_bool()
            .then(|| mi.rcWork.bottom - mi.rcWork.top)
    }
}

/// Gets the client-area height of a window.
///
/// The value uses physical pixels. The function returns 0 when the height is
/// unknown.
fn client_h(hwnd: HWND) -> i32 {
    // SAFETY: `rc` is local stack storage that the call updates.
    // A failure leaves `rc` zeroed, which gives an unknown height.
    // `GetClientRect` returns `Err` when `hwnd` is stale.
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        rc.bottom - rc.top
    }
}

fn client_w(hwnd: HWND) -> i32 {
    // SAFETY: `rc` is writable stack storage.
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        rc.right - rc.left
    }
}

fn dpi_unscale(hwnd: HWND, value: i32) -> i32 {
    let dpi = window_dpi(hwnd) as i32;
    value.saturating_mul(96).saturating_add(dpi / 2) / dpi
}

fn logical_client_w(hwnd: HWND) -> i32 {
    ((i64::from(client_w(hwnd)) * 96) / i64::from(window_dpi(hwnd))).max(1) as i32
}

fn outer_size_for_client(hwnd: HWND, width: i32, height: i32, dpi: u32) -> WinResult<POINT> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    // SAFETY: `rect` is writable stack storage.
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        AdjustWindowRectExForDpi(&mut rect, style, false, ex, dpi)?;
        if style.contains(WS_VSCROLL) {
            rect.right += GetSystemMetricsForDpi(SM_CXVSCROLL, dpi);
        }
        if style.contains(WS_HSCROLL) {
            rect.bottom += GetSystemMetricsForDpi(SM_CYHSCROLL, dpi);
        }
    }
    Ok(POINT {
        x: rect.right - rect.left,
        y: rect.bottom - rect.top,
    })
}

fn minimum_outer_size(hwnd: HWND) -> POINT {
    let width = dpi_scale(hwnd, MIN_CLIENT_W);
    let height = dpi_scale(hwnd, MIN_CLIENT_H);
    outer_size_for_client(hwnd, width, height, window_dpi(hwnd))
        .unwrap_or(POINT { x: width, y: height })
}

/// Positions the bottom row.
///
/// The row stays a fixed distance above the bottom of the client area. Tab
/// height does not affect row position. The upper area fills the space that
/// remains, so tall tabs scroll.
fn place_bottom(hwnd: HWND) {
    let ch = client_h(hwnd);
    if ch <= 0 {
        return;
    }
    let width = logical_client_w(hwnd);
    let top = ch - dpi_scale(hwnd, BOTTOM_H + PAD);
    let apply_x = width - PAD - 144;
    // SAFETY: Every handle is a live child; dimensions use the root's current DPI.
    unsafe {
        for (id, x, dy) in BOTTOM_ROW {
            let Ok(c) = GetDlgItem(Some(hwnd), id) else {
                continue;
            };
            let (control_w, control_h) = match id {
                ID_UPDATES => (width - 2 * PAD, BOTTOM_H - 8),
                ID_APPLY_STATE | ID_RUNTIME_STATUS => (width - 2 * PAD - 16, ROW_H),
                ID_STATUS => (width - 2 * PAD - 16, STATUS_H),
                ID_CHECK_UPDATE => (136, ROW_H),
                ID_APPLY => (136, ROW_H + 4),
                ID_QUIT => (116, ROW_H + 4),
                _ => continue,
            };
            let _ = SetWindowPos(
                c,
                None,
                dpi_scale(hwnd, if id == ID_APPLY { apply_x } else { x }),
                top + dpi_scale(hwnd, dy),
                dpi_scale(hwnd, control_w.max(1)),
                dpi_scale(hwnd, control_h),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if let Ok(tab) = GetDlgItem(Some(hwnd), ID_TAB) {
            let _ = SetWindowPos(tab, None, dpi_scale(hwnd, PAD - 6),
                dpi_scale(hwnd, PAD), dpi_scale(hwnd, (width - 2 * PAD).max(1)),
                dpi_scale(hwnd, TAB_H), SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let Ok((viewport, _)) = panes(hwnd) else {
            return;
        };
        let band = (top - dpi_scale(hwnd, CONTENT_Y)).max(0);
        let _ = SetWindowPos(
            viewport,
            None,
            0,
            dpi_scale(hwnd, CONTENT_Y),
            client_w(hwnd),
            band,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        // The page position changed, so repage the viewport.
        repage(hwnd, viewport);
    }
}

/// Updates the page size after a window resize.
///
/// The function keeps the scroll range and updates the page size to the new
/// band height. The scrollbar remains the single source of truth.
fn repage(hwnd: HWND, viewport: HWND) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_POS,
        ..Default::default()
    };
    // SAFETY: `si` starts with its own size and the call receives a mutable
    // pointer. `set_scroll_range` stores the height as `nMax + 1`, so this
    // read returns the exact height.
    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut si) }.is_err() {
        return;
    }
    set_scroll_range(hwnd, si.nMax + 1, client_h(viewport), si.nPos);
}

/// Scrolls the content pane vertically.
///
/// `y` is a physical-pixel coordinate that is less than or equal to 0.
fn move_content(hwnd: HWND, y: i32) {
    // SAFETY: `panes` returns `Err` on failure. The returned pane is a
    // valid descendant of `hwnd` that stays valid until window destruction.
    // `SWP_NOSIZE` keeps the band height, and `SWP_NOZORDER` keeps
    // the z-order position from `place_viewport`.
    unsafe {
        let Ok((_, content)) = panes(hwnd) else {
            return;
        };
        let _ = SetWindowPos(
            content,
            None,
            0,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Recalculates the scrollbar range.
///
/// The dimensions use physical pixels. `content_h` gives the selected tab
/// height, and `view_h` comes from the viewport.
/// The scrollbar keeps its width on short pages to keep native client bounds stable.
fn set_scroll_range(hwnd: HWND, content_h: i32, view_h: i32, position: i32) {
    let si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
        nMin: 0,
        nMax: content_h.max(1) - 1,
        nPage: view_h.max(1) as u32,
        nPos: position.clamp(0, (content_h.max(1) - view_h.max(1)).max(0)),
        ..Default::default()
    };
    // SAFETY: `hwnd` is the settings window. `si` is initialized and passed
    // as a const pointer. `SetScrollInfo` reads `si` during the call.
    let actual = unsafe { SetScrollInfo(hwnd, SB_VERT, &si, true) };
    move_content(hwnd, -actual);
}

fn scroll_position(hwnd: HWND) -> i32 {
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS,
        ..Default::default()
    };
    // SAFETY: `info` is initialized writable stack storage.
    unsafe { let _ = GetScrollInfo(hwnd, SB_VERT, &mut info); }
    info.nPos
}

/// Moves the scroll position.
///
/// `pick` reads current scroll info and selects the target position.
fn scroll_to(hwnd: HWND, pick: impl FnOnce(&SCROLLINFO) -> i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        ..Default::default()
    };
    // SAFETY: The code initializes `si` with its own size and passes a mutable
    // pointer. The returned position remains the scrollbar's single source of truth.
    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut si) }.is_err() {
        return;
    }
    let old = si.nPos;
    // A negative value means that content fits without a scrollbar.
    let max = (si.nMax - si.nPage as i32 + 1).max(0);
    si.nPos = pick(&si).clamp(0, max);
    if si.nPos == old {
        return;
    }
    si.fMask = SIF_POS;
    // SAFETY: `SetScrollInfo` has the same contract as `set_scroll_range`.
    unsafe { SetScrollInfo(hwnd, SB_VERT, &si, true) };
    move_content(hwnd, -si.nPos);
}

thread_local! {
    // Stores the queued outcome for an `HWND`.
    static OUTCOME: Cell<Option<(isize, SettingsOutcome)>> = const { Cell::new(None) };

    // Stores a queued Add or Remove action.
    static ACTION: Cell<Option<(isize, Action)>> = const { Cell::new(None) };

    // Stores a queued Anki or update click event.
    static CLICK: Cell<Option<(isize, SettingsClick)>> = const { Cell::new(None) };

    // Stores a queued tab switch index.
    static TAB: Cell<Option<(isize, u32)>> = const { Cell::new(None) };

    // Stores key capture state for a window handle and control identifier.
    static CAPTURING: Cell<Option<(isize, i32)>> = const { Cell::new(None) };

    // Stores button text before key capture.
    static CAPTURE_PREV: RefCell<Option<(isize, String)>> = const { RefCell::new(None) };

    // Stores the captured virtual key code for each `HWND`.
    static CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Stores the Anki add virtual key code for each `HWND`.
    static ANKI_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Stores the Static region virtual key code for each `HWND`.
    static SR_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };

    // Stores the OCR clipboard virtual key code for each `HWND`.
    static OCR_CLIP_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };
    static SCREENSHOT_CAPTURED_VK: Cell<Option<(isize, u16)>> = const { Cell::new(None) };
    static SEARCH_CAPTURED: RefCell<Option<(isize, String)>> = const { RefCell::new(None) };
    static SENTENCE_SEARCH_CAPTURED: RefCell<Option<(isize, String)>> = const { RefCell::new(None) };
    static SEARCH_REQUEST: Cell<Option<(isize, chibipop::search::SearchMode)>> = const { Cell::new(None) };

    // Stores the field-map toggle click flag for each `HWND`.
    static FIELD_MAP_TOGGLE: Cell<Option<isize>> = const { Cell::new(None) };

    // Stores a queued Anki model selection.
    static ANKI_MODEL_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    // Stores a queued OCR language selection.
    static LANG_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    static CONDITION_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    static CONDITIONAL_TABS: RefCell<Option<(isize, ConditionalTabs)>> =
        const { RefCell::new(None) };

    // Stores plugin directories for each `HWND`.
    static PLUGIN_DIRS: RefCell<Option<(isize, Vec<PathBuf>)>> = const { RefCell::new(None) };

    // Stores active drag-row state for each `HWND`.
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };

    static RESIZED: Cell<Option<isize>> = const { Cell::new(None) };
    static WINDOW_DPI: Cell<Option<(isize, u32)>> = const { Cell::new(None) };
    static SHOW_LOGS: Cell<Option<isize>> = const { Cell::new(None) };
    static USER_EDIT: Cell<Option<isize>> = const { Cell::new(None) };
    static EDIT_TRACKING: Cell<Option<isize>> = const { Cell::new(None) };
}

fn record_user_edit(hwnd: HWND) {
    let owner = hwnd.0 as isize;
    let active = EDIT_TRACKING.with(|slot| slot.get() == Some(owner));
    if active {
        USER_EDIT.with(|slot| slot.set(Some(owner)));
    }
}

fn without_edit_tracking(hwnd: HWND, action: impl FnOnce()) {
    let owner = hwnd.0 as isize;
    let active = EDIT_TRACKING.with(|slot| {
        let active = slot.get() == Some(owner);
        if active {
            slot.set(None);
        }
        active
    });
    action();
    if active {
        EDIT_TRACKING.with(|slot| slot.set(Some(owner)));
    }
}

fn user_edit_command(id: i32, notify: u16) -> bool {
    if matches!(id, 1 | 2) {
        return false;
    }
    if (ID_FIELD_MAP_BASE..ID_FIELD_MAP_BASE + 100).contains(&id) {
        return notify == CBN_SELCHANGE as u16;
    }
    if (ID_PLUGIN_ENABLE_BASE..ID_PLUGIN_ENABLE_BASE + PLUGIN_ID_SPAN).contains(&id) {
        return notify == BN_CLICKED as u16;
    }
    if matches!(
        id,
        ID_APPLY | ID_QUIT | ID_CHECK_UPDATE | ID_ANKI_TEST | ID_CSS_EDITOR
            | ID_ENGINE_CONFIGURE | ID_FIELD_MAP_TOGGLE | ID_SHOW_LIVE_LOGS
            | ID_TRIGGER_KEY | ID_ANKI_ADD_KEY | ID_STATIC_REGION_KEY
            | ID_OCR_CLIPBOARD_KEY | ID_SCREENSHOT_HOTKEY | ID_STATUS
            | ID_APPLY_STATE | ID_RUNTIME_STATUS
            | ID_SEARCH_KEY | ID_SENTENCE_SEARCH_KEY
            | ID_OPEN_DICTIONARY_SEARCH | ID_OPEN_SENTENCE_SEARCH
    ) {
        return false;
    }
    matches!(notify as u32, BN_CLICKED | CBN_SELCHANGE | CBN_EDITCHANGE | EN_CHANGE)
}

fn remember_conditional_tabs(hwnd: HWND, tabs: ConditionalTabs) {
    CONDITIONAL_TABS.with(|slot| *slot.borrow_mut() = Some((hwnd.0 as isize, tabs)));
}

fn conditional_tabs(hwnd: HWND) -> ConditionalTabs {
    CONDITIONAL_TABS.with(|slot| match *slot.borrow() {
        Some((owner, tabs)) if owner == hwnd.0 as isize => tabs,
        _ => ConditionalTabs::default(),
    })
}

fn record_outcome(hwnd: HWND, outcome: SettingsOutcome) {
    OUTCOME.with(|c| c.set(Some((hwnd.0 as isize, outcome))));
}

fn record_action(hwnd: HWND, action: Action) {
    ACTION.with(|c| c.set(Some((hwnd.0 as isize, action))));
    // SAFETY: The window procedure handles `hwnd`, so it stays valid during
    // this call. WM_NULL has no payload, and `DefWindowProcW` discards it.
    // WM_NULL wakes `GetMessageW`, so `pump` runs immediately.
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

fn record_anki_model_change(hwnd: HWND) {
    ANKI_MODEL_CHANGED.with(|c| c.set(Some(hwnd.0 as isize)));
    // SAFETY: The window procedure handles `hwnd`, so it stays valid during
    // this call. WM_NULL wakes the application message pump.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

fn remember_plugin_dirs(hwnd: HWND, dirs: Vec<PathBuf>) {
    PLUGIN_DIRS.with(|c| *c.borrow_mut() = Some((hwnd.0 as isize, dirs)));
}

/// Returns the directory for a Configure button identifier.
fn plugin_dir_at(hwnd: HWND, idx: usize) -> Option<PathBuf> {
    PLUGIN_DIRS.with(|c| match &*c.borrow() {
        Some((h, dirs)) if *h == hwnd.0 as isize => dirs.get(idx).cloned(),
        _ => None,
    })
}

/// Returns the index for a Configure button.
fn plugin_configure_idx(id: i32) -> Option<usize> {
    (ID_PLUGIN_CONFIGURE_BASE..ID_PLUGIN_CONFIGURE_BASE + PLUGIN_ID_SPAN)
        .contains(&id)
        .then(|| (id - ID_PLUGIN_CONFIGURE_BASE) as usize)
}

/// Opens the directory in File Explorer.
unsafe fn open_plugin_dir(hwnd: HWND, idx: usize) {
    let Some(dir) = plugin_dir_at(hwnd, idx) else {
        return;
    };
    let path = wide(&dir.to_string_lossy());
    // SAFETY: `path` is a null-terminated UTF-16 string valid for the
    // call. The operating system only reads the buffer. An invalid path
    // fails to open and does not cause undefined behavior.
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

/// Sets the folder path or appends it when absent.
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

/// Selects a folder with a file dialog.
///
/// Returns `None` when the user cancels the dialog.
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
    // SAFETY: `ofn` contains pointers to buffers that outlive this call.
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
    // SAFETY: The window procedure handles `hwnd`, so it stays valid during
    // this call. WM_NULL has no payload, and `DefWindowProcW` discards it.
    // WM_NULL wakes `GetMessageW`, so `pump` updates the list.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

/// Starts key capture mode.
unsafe fn begin_capture(hwnd: HWND, id: i32) {
    // SAFETY: `id` is a key capture button identifier and a valid
    // descendant of `hwnd`. `window_text` and `SetWindowTextW` define
    // their own safety contracts.
    unsafe {
        let Ok(btn) = dlg_item(hwnd, id) else { return };
        let prev = window_text(btn);
        CAPTURE_PREV.with(|c| *c.borrow_mut() = Some((hwnd.0 as isize, prev)));
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, id))));
        let prompt = if matches!(id, ID_SEARCH_KEY | ID_SENTENCE_SEARCH_KEY) { w!("Press keys (Esc cancels)") }
            else if id == ID_SCREENSHOT_HOTKEY { w!("Press one key (Esc cancels)") }
            else { w!("Press a key...") };
        let _ = SetWindowTextW(btn, prompt);
    }
}

unsafe fn clear_captured_key(
    hwnd: HWND,
    id: i32,
    cell: &'static std::thread::LocalKey<Cell<Option<(isize, u16)>>>,
) {
    // SAFETY: Capture state belongs to this live window.
    unsafe { cancel_capture(hwnd) };
    cell.with(|slot| slot.set(Some((hwnd.0 as isize, 0))));
    // SAFETY: `id` names a live key button owned by `hwnd`.
    unsafe {
        if let Ok(button) = dlg_item(hwnd, id) {
            let _ = SetWindowTextW(button, w!("Not set"));
        }
    }
}

/// Ends key capture mode without changes.
unsafe fn cancel_capture(hwnd: HWND) {
    // SAFETY: `id` originates from `CAPTURING`, which `begin_capture` sets
    // to a valid descendant of `hwnd`. The saved text originates from that
    // same control.
    unsafe {
        let mine = hwnd.0 as isize;
        let captured = CAPTURING
            .with(|c| c.get())
            .and_then(|(h, id)| (h == mine).then_some(id));
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
            // Any mouse click cancels key capture mode.
            unsafe { cancel_capture(hwnd) };
            if user_edit_command(id, notify) {
                record_user_edit(hwnd);
            }
            // Role lists report events through WM_NOTIFY. List events do not
            // arrive here. `SECTIONS` defines each button association.
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
                CONDITION_CHANGED.with(|c| c.set(Some(hwnd.0 as isize)));
                unsafe { update_static_controls(hwnd) };
                return LRESULT(0);
            }
            if id == ID_ANKI_MODEL && notify == CBN_SELCHANGE as u16 {
                record_anki_model_change(hwnd);
                return LRESULT(0);
            }
            if let Some(idx) = plugin_configure_idx(id) {
                unsafe { open_plugin_dir(hwnd, idx) };
                return LRESULT(0);
            }
            match id {
                // The value 1 represents IDOK from the Enter key, not a control identifier.
                ID_APPLY | 1 => record_outcome(hwnd, SettingsOutcome::Apply),
                // The value 2 represents the Escape key. Window close uses WM_CLOSE.
                2 => record_outcome(hwnd, SettingsOutcome::Cancel),
                ID_QUIT => record_outcome(hwnd, SettingsOutcome::Quit),
                ID_ENGINE_CONFIGURE => record_action(hwnd, Action::ConfigureEngine),
                ID_ANKI_TEST => record_click(hwnd, SettingsClick::AnkiTest),
                ID_CHECK_UPDATE => record_click(hwnd, SettingsClick::CheckUpdate),
                ID_CSS_EDITOR => record_click(hwnd, SettingsClick::CssEditor),
                ID_SHOW_LIVE_LOGS => SHOW_LOGS.with(|slot| slot.set(Some(hwnd.0 as isize))),
                ID_SCREENSHOT_RESET => record_action(hwnd, Action::ResetScreenshotTargets),
                ID_FIELD_MAP_TOGGLE => record_field_map_toggle(hwnd),
                ID_MODE_LIVE | ID_MODE_HOLD | ID_MODE_TOGGLE | ID_MODE_PRESS => unsafe {
                    if let Ok(c) = dlg_item(hwnd, ID_TRIGGER_KEY) {
                        let _ = EnableWindow(c, id != ID_MODE_LIVE);
                    }
                    if let Ok(c) = dlg_item(hwnd, ID_PER_CHAR) {
                        let _ = EnableWindow(c, id == ID_MODE_LIVE);
                    }
                },
                ID_TRIGGER_KEY => unsafe { begin_capture(hwnd, ID_TRIGGER_KEY) },
                ID_SEARCH_KEY | ID_SENTENCE_SEARCH_KEY => unsafe { begin_capture(hwnd, id) },
                ID_SEARCH_KEY_CLEAR | ID_SENTENCE_SEARCH_KEY_CLEAR => {
                    let target = if id == ID_SEARCH_KEY_CLEAR { ID_SEARCH_KEY } else { ID_SENTENCE_SEARCH_KEY };
                    set_search_key(hwnd, target, String::new());
                }
                ID_OPEN_DICTIONARY_SEARCH | ID_OPEN_SENTENCE_SEARCH => {
                    let mode = if id == ID_OPEN_DICTIONARY_SEARCH { chibipop::search::SearchMode::Dictionary }
                        else { chibipop::search::SearchMode::Sentence };
                    SEARCH_REQUEST.with(|cell| cell.set(Some((hwnd.0 as isize, mode))));
                }
                ID_ANKI_ADD_KEY => unsafe { begin_capture(hwnd, ID_ANKI_ADD_KEY) },
                ID_STATIC_REGION_KEY => unsafe { begin_capture(hwnd, ID_STATIC_REGION_KEY) },
                ID_OCR_CLIPBOARD_KEY => unsafe { begin_capture(hwnd, ID_OCR_CLIPBOARD_KEY) },
                ID_SCREENSHOT_HOTKEY => unsafe { begin_capture(hwnd, ID_SCREENSHOT_HOTKEY) },
                ID_SCREENSHOT_KEY_CLEAR => {
                    // SAFETY: Capture state belongs to this live window.
                    unsafe { cancel_capture(hwnd) };
                    SCREENSHOT_CAPTURED_VK.with(|c| c.set(Some((hwnd.0 as isize, 0))));
                    // SAFETY: The button is a descendant of this live settings window.
                    unsafe {
                        if let Ok(button) = dlg_item(hwnd, ID_SCREENSHOT_HOTKEY) {
                            let _ = SetWindowTextW(button, w!("Not set"));
                        }
                    }
                }
                ID_ANKI_ADD_KEY_CLEAR => unsafe {
                    clear_captured_key(hwnd, ID_ANKI_ADD_KEY, &ANKI_CAPTURED_VK);
                },
                ID_STATIC_REGION_KEY_CLEAR => unsafe {
                    clear_captured_key(hwnd, ID_STATIC_REGION_KEY, &SR_CAPTURED_VK);
                },
                ID_OCR_CLIPBOARD_KEY_CLEAR => unsafe {
                    clear_captured_key(hwnd, ID_OCR_CLIPBOARD_KEY, &OCR_CLIP_CAPTURED_VK);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: `lparam` points to an NMHDR structure or to a structure
            // with an NMHDR first member. The operating system guarantees this
            // layout for WM_NOTIFY messages.
            let nmhdr = unsafe { &*(lparam.0 as *const NmhdrRaw) };
            if nmhdr.code == TCN_SELCHANGE_CODE && nmhdr.id_from == ID_TAB as usize {
                let tab = unsafe {
                    SendMessageW(nmhdr.hwnd_from, TCM_GETCURSEL_MSG, None, None).0 as u32
                };
                TAB.with(|c| c.set(Some((hwnd.0 as isize, tab))));
            }
            // Arrow keys and clicks change row selection. The space bar and
            // checkbox clicks change checkbox state. Both actions trigger
            // this notification. Only selection changes affect Move button
            // state, but one branch updates buttons for either change.
            // The control stores enabled state until `read` queries it.
            if nmhdr.code == LVN_ITEMCHANGED
                && section_of_list(nmhdr.id_from as i32).is_some()
            {
                // SAFETY: LVN_ITEMCHANGED supplies NMLISTVIEW.
                let item = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                if (item.uOldState ^ item.uNewState) & LVIS_STATEIMAGEMASK.0 != 0 {
                    record_user_edit(hwnd);
                }
                unsafe { update_list_buttons(hwnd) };
            }
            // Drag operations start here and continue below. The control detects
            // a drag gesture but does not track movement.
            // The window procedure tracks the rest of the gesture. `SECTIONS` identifies
            // the target section. A drop cannot enter another list.
            if nmhdr.code == LVN_BEGINDRAG {
                if let Some(section) = section_of_list(nmhdr.id_from as i32) {
                    // SAFETY: For LVN_BEGINDRAG, `lparam` points to an
                    // NMLISTVIEW structure whose first member is NMHDR.
                    // The control guarantees this layout for this notification.
                    let nm = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                    let origin = (nm.ptAction.x, nm.ptAction.y);
                    unsafe { begin_drag(hwnd, section, nm.iItem, origin) };
                }
            }
            LRESULT(0)
        }
        // These three messages apply only during an active row drag.
        // Without an active drag, the default handler processes them.
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
            if wparam.0 != SIZE_MINIMIZED as usize {
                RESIZED.with(|slot| slot.set(Some(hwnd.0 as isize)));
            }
            place_bottom(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            if dpi != 0 && lparam.0 != 0 {
                WINDOW_DPI.with(|slot| slot.set(Some((hwnd.0 as isize, dpi))));
                RESIZED.with(|slot| slot.set(Some(hwnd.0 as isize)));
                // SAFETY: WM_DPICHANGED supplies a valid RECT for this call.
                unsafe {
                    let rect = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(hwnd, None, rect.left, rect.top,
                        rect.right - rect.left, rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if lparam.0 != 0 {
                let minimum = minimum_outer_size(hwnd);
                // SAFETY: WM_GETMINMAXINFO supplies writable MINMAXINFO.
                unsafe {
                    (*(lparam.0 as *mut MINMAXINFO)).ptMinTrackSize = minimum;
                }
            }
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
            // The high word contains the signed delta. The low word contains key state.
            let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
            let step = delta / WHEEL_DELTA as i32 * WHEEL_LINES * dpi_scale(hwnd, SCROLL_LINE);
            // Wheel rotation moves the content toward the top.
            scroll_to(hwnd, |si| si.nPos - step);
            LRESULT(0)
        }
        WM_CLOSE => {
            record_outcome(hwnd, SettingsOutcome::Quit);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Registers the window class once per process.
///
/// Sets the registration flag only after success.
unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` is an initialized `WNDCLASSEXW` structure with unset fields
    // set to zero. `lpfnWndProc` points to an extern system function that stays
    // valid for the process lifetime, as the operating system requires.
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
            let error = Error::from_thread();
            if error.code() != HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0) {
                return Err(error).context("RegisterClassExW");
            }
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Handles messages for both panes.
///
/// It forwards only messages that `wndproc` handles.
unsafe extern "system" fn pane_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND | WM_NOTIFY => {
            // SAFETY: `hwnd` is a valid pane window that this module created.
            // The parent window outlives this pane. `GetParent` returns `Err`
            // on failure. The code forwards no message for an invalid handle.
            let parent = unsafe { GetParent(hwnd) };
            match parent {
                // SAFETY: `p` is the valid parent handle returned above.
                // The code passes `wparam` and `lparam` unchanged.
                // The original message semantics remain.
                Ok(p) => unsafe { SendMessageW(p, msg, Some(wparam), Some(lparam)) },
                Err(_) => LRESULT(0),
            }
        }
        // SAFETY: `DefWindowProcW` handles messages that this procedure does not handle.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Registers the pane window class once per process.
///
/// Sets the registration flag only after success.
unsafe fn register_pane_class(hinstance: HINSTANCE) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: `wc` is an initialized `WNDCLASSEXW` structure with unset fields
    // set to zero. `lpfnWndProc` points to an extern system function that stays
    // valid for the process lifetime, as the operating system requires.
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
            let error = Error::from_thread();
            if error.code() != HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0) {
                return Err(error).context("RegisterClassExW for the pane class");
            }
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Gets the system user interface font.
///
/// Returns `None` to keep the default font.
unsafe fn ui_font(dpi: u32) -> Option<HFONT> {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `ncm` is stack storage with a size that matches its `cbSize`
    // field, as the SystemParametersInfoForDpi contract requires.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            Some(&mut ncm as *mut _ as *mut core::ffi::c_void),
            0,
            dpi,
        )
    }
    .is_ok();
    if !ok {
        return None;
    }
    // SAFETY: `SystemParametersInfoForDpi` populated `lfMessageFont` above.
    let font = unsafe { CreateFontIndirectW(&ncm.lfMessageFont) };
    if font.is_invalid() {
        None
    } else {
        Some(font)
    }
}

/// Lists fonts that can display Japanese kana glyphs.
///
/// Glyph coverage is not guaranteed.
/// A font name that starts with `@` represents a vertical layout variant.
pub fn japanese_font_families() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // SAFETY: `lf` and `out` are local storage that outlives the call.
    // `EnumFontFamiliesExW` calls the callback synchronously on this thread,
    // so its `&mut Vec` stays valid. `ReleaseDC` runs on every path.
    unsafe {
        let hdc = GetDC(None);
        let lf = LOGFONTW {
            lfCharSet: SHIFTJIS_CHARSET,
            ..Default::default()
        };
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
    // SAFETY: The operating system passes a valid `ENUMLOGFONTEXW` pointer
    // and the `lparam` value. `japanese_font_families` supplies an
    // `&mut Vec<String>` pointer that outlives font enumeration.
    unsafe {
        let elf = &*(lf as *const ENUMLOGFONTEXW);
        let name = String::from_utf16_lossy(
            &elf.elfLogFont.lfFaceName[..elf
                .elfLogFont
                .lfFaceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(0)],
        );
        if !name.is_empty() && !name.starts_with('@') {
            (*(lparam.0 as *mut Vec<String>)).push(name);
        }
    }
    1
}

/// Builds combo box rows with a name and tag.
///
/// Decision D4: The function preserves and marks absent tags.
fn language_choices(installed: Vec<(String, String)>, configured: &str) -> Vec<(String, String)> {
    let mut out = installed;
    if !configured.is_empty() && !out.iter().any(|(_, tag)| tag_matches(tag, configured)) {
        out.push((
            format!("{configured} (not installed)"),
            configured.to_string(),
        ));
    }
    out
}

/// Returns the row index that contains the `configured` value.
fn language_index(rows: &[(String, String)], configured: &str) -> Option<usize> {
    if configured.is_empty() {
        return None;
    }
    rows.iter()
        .position(|(_, tag)| tag_matches(tag, configured))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Creates a child control with the standard user interface font.
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
    // SAFETY: `parent` is a valid window handle. The operating system copies
    // `text` during the call. `id` acts as the child window identifier menu parameter.
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
        // SAFETY: `hwnd` was created above. `WM_SETFONT` performs no pointer
        // copy, and the font outlives the window until destruction in `Drop`.
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(f.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
    Ok(hwnd)
}

/// Finds a control by identifier across panes.
///
/// `GetDlgItem` inspects direct child windows only. Tab page controls are
/// child windows of the internal viewport panes.
unsafe fn dlg_item(root: HWND, id: i32) -> WinResult<HWND> {
    // SAFETY: `root` is a valid window handle. The function checks each
    // `GetDlgItem` result and returns `Err` when a control is absent.
    // Callers supply unique non-zero `ID_*` constants. Shared identifier 0
    // exists only on the content pane, so the root search finds the correct control.
    unsafe {
        if let Ok(c) = GetDlgItem(Some(root), id) {
            return Ok(c);
        }
        let (_, content) = panes(root)?;
        GetDlgItem(Some(content), id)
    }
}

/// Returns the viewport pane and content pane.
unsafe fn panes(root: HWND) -> WinResult<(HWND, HWND)> {
    // SAFETY: `root` is a valid window handle. The function checks both
    // lookup results. Windows without initialized panes return `Err` safely.
    unsafe {
        let viewport = GetDlgItem(Some(root), ID_VIEWPORT)?;
        let content = GetDlgItem(Some(viewport), ID_CONTENT)?;
        Ok((viewport, content))
    }
}

/// Converts a state image index to ListView item state with a bit shift.
///
/// ListView items have no independent checkbox field. State image 1 represents
/// cleared, and state image 2 represents checked. The value shifts into the
/// mask that `LVIS_STATEIMAGEMASK` defines. The Windows SDK defines this
/// operation as the `INDEXTOSTATEIMAGEMASK` macro.
const LV_STATE_IMAGE_SHIFT: u32 = 12;

/// Returns the state image index for a checked or cleared row.
fn check_state(checked: bool) -> u32 {
    let index: u32 = if checked { 2 } else { 1 };
    index << LV_STATE_IMAGE_SHIFT
}

/// Checks whether item state has a checked checkbox.
///
/// Other values mean cleared. Value 0 occurs when a row was inserted before
/// extended checkbox style initialization.
fn state_is_checked(state: u32) -> bool {
    state & LVIS_STATEIMAGEMASK.0 == check_state(true)
}

/// Creates an empty role ListView control for new items.
///
/// The control uses report view with one untitled column. Report view is
/// required for `LVS_EX_CHECKBOXES` checkboxes, and column 0 displays row text.
/// The code applies the extended style before it adds rows, so comctl32
/// initializes the state image list correctly.
unsafe fn make_role_list(
    parent: HWND,
    y: i32,
    w: i32,
    id: i32,
    font: Option<HFONT>,
) -> WinResult<HWND> {
    // SAFETY: `parent` is a valid pane owned by the caller. `child` creates
    // the control. Windows messages target the new control, and parameter
    // structures are initialized with Default.
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
        // The single column fills the full client width.
        // Without this width, the control truncates long dictionary names.
        SendMessageW(list, LVM_SETCOLUMNWIDTH, Some(WPARAM(0)),
            Some(LPARAM(LVSCW_AUTOSIZE_USEHEADER as isize)));
        // The control draws the drag insertion mark with the configured color.
        // The default fixed color can disappear against dark theme rows.
        // Row text color follows the active user theme.
        SendMessageW(list, LVM_SETINSERTMARKCOLOR, None,
            Some(LPARAM(GetSysColor(COLOR_WINDOWTEXT) as isize)));
        Ok(list)
    }
}

/// Gets the text of one ListView row.
///
/// Win32 ListView controls provide no text length query message.
/// The code enlarges the buffer until the control returns the complete text.
/// Dictionaries use exact names. Truncated text can reference the wrong
/// Dictionary. Refer to ARCHITECTURE.md#dictionary-and-lookup.
unsafe fn lv_text(list: HWND, index: i32) -> String {
    // SAFETY: `list` is a valid ListView handle. `item` is initialized
    // with `pszText` set to `buf`. `buf` outlives the call, and
    // `cchTextMax` specifies buffer capacity for LVM_GETITEMTEXTW.
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
            // A full buffer can indicate truncation.
            // The limit of 64 Ki wide characters bounds allocation size.
            if copied + 1 < buf.len() || buf.len() >= 1 << 16 {
                return String::from_utf16_lossy(&buf[..copied]);
            }
            buf = vec![0u16; buf.len() * 2];
        }
    }
}

/// Checks whether a row checkbox is checked.
unsafe fn lv_checked(list: HWND, index: i32) -> bool {
    // SAFETY: `list` is a valid ListView handle. LVM_GETITEMSTATE takes
    // the row index in `wparam` and the mask in `lparam`. The call returns
    // the masked state. The call passes no pointer arguments.
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

/// Returns the total number of rows in the list.
unsafe fn lv_count(list: HWND) -> i32 {
    // SAFETY: `list` is a valid ListView handle. LVM_GETITEMCOUNT passes
    // no pointer arguments.
    unsafe { SendMessageW(list, LVM_GETITEMCOUNT, None, None).0 as i32 }
}

/// Returns the selected row index, or -1 when no row is selected.
unsafe fn lv_selection(list: HWND) -> i32 {
    // SAFETY: `list` is a valid ListView handle. LVM_GETNEXTITEM takes
    // the previous index in `wparam` (-1 searches from start) and returns
    // a row index or -1.
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

/// Gets the row name and checkbox state.
unsafe fn lv_row(list: HWND, index: i32) -> DictRow {
    // SAFETY: `lv_text` and `lv_checked` meet their safety conditions.
    unsafe { DictRow { name: lv_text(list, index), enabled: lv_checked(list, index) } }
}

/// Gets all rows of a role list, or `None` when the control is absent.
///
/// `id` identifies a descendant of `hwnd`. Absent controls return `Err`, and
/// `lv_row` meets its safety conditions.
unsafe fn lv_rows(hwnd: HWND, id: i32) -> Option<Vec<DictRow>> {
    unsafe {
        let list = dlg_item(hwnd, id).ok()?;
        Some((0..lv_count(list).max(0)).map(|i| lv_row(list, i)).collect())
    }
}

/// Finds the row index that matches `name`, if present.
///
/// The function compares exact string equality and does not call LVM_FINDITEMW.
/// Configuration lookup requires exact dictionary names.
/// Refer to ARCHITECTURE.md#dictionary-and-lookup.
unsafe fn lv_find(list: HWND, name: &str) -> Option<i32> {
    // SAFETY: `list` is a valid ListView handle owned by the caller.
    // `lv_text` meets its safety conditions.
    unsafe { (0..lv_count(list).max(0)).find(|&i| lv_text(list, i) == name) }
}

/// Replaces text and checkbox state of a row in place.
///
/// A reorder swaps both text and checkbox states. If the code exchanged text
/// alone, it would attach checkbox state to the wrong Dictionary.
unsafe fn lv_set(list: HWND, index: i32, row: &DictRow) {
    // SAFETY: `list` is a valid ListView handle. `item` is initialized,
    // and `pszText` points to valid memory copied during the call.
    // `lv_check` meets its safety conditions.
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

/// Sets or clears the checkbox for one row.
///
/// Runs after a row is added. With `LVS_EX_CHECKBOXES`, comctl32
/// assigns state image 1 to a new item and overwrites the state passed at insert.
unsafe fn lv_check(list: HWND, index: i32, checked: bool) {
    // SAFETY: `list` is a valid ListView handle. `item` is initialized
    // and contains no external pointer fields.
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

/// Appends one row to the list and returns its index.
unsafe fn lv_append(list: HWND, row: &DictRow) -> i32 {
    // SAFETY: `list` is a valid ListView handle. `item` is initialized,
    // and `pszText` points to valid memory copied during the call.
    // `lv_check` meets its safety conditions.
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

/// Selects and scrolls to `index`, or clears selection when index is less than 0.
///
/// The function clears all row selections first. LVM_SETITEMSTATE with row -1
/// updates all rows. The first clear avoids multiple selections that confuse Move buttons.
unsafe fn lv_select(list: HWND, index: i32) {
    // SAFETY: `list` is a valid ListView handle. Both structures are
    // initialized and carry no pointer members.
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

/// Refills the list and selects `at` or the last row if the list is shorter.
unsafe fn fill_role_list(list: HWND, rows: &[DictRow], at: i32) {
    // SAFETY: `list` is a valid ListView handle. `lv_append` and
    // `lv_select` meet their safety conditions.
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for row in rows {
            lv_append(list, row);
        }
        lv_select(list, at.min(rows.len() as i32 - 1));
    }
}

/// Calculates the target row index for a move operation within one list.
///
/// Each role has its own list, so a move swaps an item with its neighbor.
/// An empty enabled list is a valid configuration that searches no dictionaries.
/// The code does not force a list to contain an item.
/// Refer to ARCHITECTURE.md#dictionary-and-lookup.
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

/// Checks whether a section Move button can move a row.
///
/// `count` and `selection` come from the ListView control. A negative
/// selection means no selected row, so both Move buttons stay disabled.
fn can_move(count: i32, selection: i32, up: bool) -> bool {
    match (usize::try_from(count), usize::try_from(selection)) {
        (Ok(len), Ok(index)) => move_target(len, index, up).is_some(),
        _ => false,
    }
}

/// Reorders rows within one section.
///
/// The function swaps rows in place. It does not refill the list.
/// The selection follows the moved item. A refill would clear selection
/// briefly and remove focus from the active Move button.
unsafe fn move_selected(hwnd: HWND, section: &Section, up: bool) {
    // SAFETY: `section.list` identifies a valid descendant of `hwnd`
    // created in `build`. Absent controls return `Err`, and all
    // `lv_*` helpers meet their safety conditions.
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

/// Disables buttons that cannot act.
///
/// Focus moves to the parent list before the button is disabled.
/// A disabled control retains Windows focus and drops keyboard input.
/// The code moves focus so keyboard navigation does not stop on a disabled button.
unsafe fn update_list_buttons(hwnd: HWND) {
    // SAFETY: Each identifier below names a valid descendant of `hwnd`
    // created in `build`. Each `dlg_item` lookup is validated.
    unsafe {
        // Read focus once. Only one control has focus, so later iterations
        // cannot match a focus handle that already moved.
        let focused = GetFocus();
        for section in &SECTIONS {
            let Ok(list) = dlg_item(hwnd, section.list) else { continue };
            let count = lv_count(list);
            let cur = lv_selection(list);
            for (id, enable) in [
                (section.up, can_move(count, cur, true)),
                (section.down, can_move(count, cur, false)),
                // Remove needs only one row. Unreadable archives appear
                // in Terms without roles, so users can remove them.
                // Refer to ARCHITECTURE.md#dictionary-and-lookup.
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

/// Stores active drag state: the source row, source list, and initial cursor position.
///
/// `origin` stores `NMLISTVIEW::ptAction` in list client coordinates.
/// Later cursor readings use this coordinate space. The state keeps the section
/// so a drag cannot drop into a different list.
#[derive(Clone, Copy)]
struct Drag {
    window: isize,
    section: &'static Section,
    from: i32,
    origin: (i32, i32),
}

/// Minimum cursor travel in pixels that starts a reorder drag.
///
/// Rows have checkboxes, so a click can toggle a checkbox instead of a drag.
/// A slight mouse move during a click stays a click. The threshold starts a drag
/// only after enough travel.
/// comctl32 checks `SM_CXDRAG`, and `drop_gap` needs half a row movement.
/// The threshold adds a third check.
const DRAG_DEADBAND_PX: i32 = 5;

/// Checks whether the cursor moved past the drag threshold from press origin.
///
/// A move beyond the threshold on either axis starts the drag.
/// A check on both axes would reject vertical drags.
fn clears_drag_deadband(origin: (i32, i32), now: (i32, i32)) -> bool {
    (now.0 - origin.0).abs() >= DRAG_DEADBAND_PX
        || (now.1 - origin.1).abs() >= DRAG_DEADBAND_PX
}

/// Returns the row gap index under the cursor, in the range `0..=rows`.
///
/// `top` is the top coordinate of row 0 in list client coordinates.
/// The nearest boundary selects the insertion mark position. The clamp
/// restricts the drag to this section. Cursors outside the list clamp
/// to the first or last gap. Refer to ARCHITECTURE.md#dictionary-and-lookup.
fn drop_gap(y: i32, top: i32, row_h: i32, rows: i32) -> i32 {
    if row_h <= 0 || rows <= 0 {
        return 0;
    }
    // The code rounds to the nearest boundary rather than truncation.
    // Offsets above row 0 are negative, and truncation toward zero selects
    // the incorrect gap.
    let offset = y - top;
    (offset * 2 + row_h).div_euclid(row_h * 2).clamp(0, rows)
}

/// Returns the target row index when the code drops `from` into `gap`.
///
/// The source row leaves the list before the move, so later gap indices shift
/// down by one. A gap above `from` has the row index that it covers.
fn drop_target(from: i32, gap: i32) -> i32 {
    if gap > from {
        gap - 1
    } else {
        gap
    }
}

/// Returns the insertion mark location: row index and boundary side.
///
/// The control defines an insertion mark by row and side. Gaps select
/// before the row, except the final gap, which selects after the last row.
fn insert_mark_at(gap: i32, rows: i32) -> (i32, u32) {
    if gap >= rows {
        (rows - 1, LVIM_AFTER)
    } else {
        (gap, 0)
    }
}

/// Returns the top coordinate of row 0 and the row height in list client
/// coordinates.
///
/// The function computes these values from row 0 bounds. The top coordinate
/// represents the scroll offset, and report view rows share equal height.
/// The function returns `None` when the list contains no rows.
unsafe fn lv_row_metrics(list: HWND) -> Option<(i32, i32)> {
    // SAFETY: `list` is a valid ListView handle. `rect` is local stack
    // storage that outlives the call. LVM_GETITEMRECT reads the requested
    // part code from `left` before it writes the output.
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

/// Returns the cursor position in control client coordinates.
///
/// The function gets the mouse position with GetCursorPos. A captured drag
/// reports coordinates relative to the capture window, but drop calculations
/// require list client coordinates.
unsafe fn cursor_in(ctrl: HWND) -> (i32, i32) {
    // SAFETY: `pt` is local stack storage for both calls. `ctrl` is a
    // valid control handle. If GetCursorPos fails, `pt` remains zeroed.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = ScreenToClient(ctrl, &mut pt);
        (pt.x, pt.y)
    }
}

/// Draws the insertion mark at `at` or removes it when `None`.
unsafe fn lv_insert_mark(list: HWND, at: Option<(i32, u32)>) {
    // SAFETY: `list` is a valid ListView handle. `mark` is an initialized
    // structure with explicit size and no pointer fields.
    unsafe {
        // Row -1 means no insertion mark. The clear and end paths send this value.
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

/// Returns the active drag operation in this window.
fn drag_of(hwnd: HWND) -> Option<Drag> {
    DRAG.with(|c| c.get()).filter(|d| d.window == hwnd.0 as isize)
}

/// Starts a row drag and captures mouse input.
///
/// The settings window captures the mouse instead of the ListView.
/// The window procedure processes movement and button release messages.
/// comctl32 issues `LVN_BEGINDRAG` and does not track further mouse motion.
unsafe fn begin_drag(hwnd: HWND, section: &'static Section, from: i32, origin: (i32, i32)) {
    // SAFETY: `hwnd` has a window procedure, so it stays valid during
    // the `SetCapture` call.
    unsafe {
        if from < 0 {
            return;
        }
        let window = hwnd.0 as isize;
        DRAG.with(|c| c.set(Some(Drag { window, section, from, origin })));
        SetCapture(hwnd);
    }
}

/// Updates insertion mark location based on current cursor position.
unsafe fn track_drag(hwnd: HWND) {
    // SAFETY: `drag.section.list` names a valid descendant of `hwnd`
    // created in `build`. Each helper meets its safety conditions.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        let Ok(list) = dlg_item(hwnd, drag.section.list) else { return };
        let now = cursor_in(list);
        let rows = lv_count(list);
        // Movement below the threshold remains a click, so no insertion
        // mark appears. Checkbox clicks must not trigger move marks.
        let at = if clears_drag_deadband(drag.origin, now) {
            lv_row_metrics(list)
                .map(|(top, row_h)| insert_mark_at(drop_gap(now.1, top, row_h, rows), rows))
        } else {
            None
        };
        lv_insert_mark(list, at);
    }
}

/// Releases mouse capture without a row-order change.
///
/// When another component takes capture, this function cancels the drag
/// gesture. It removes the insertion mark and keeps the original row order.
unsafe fn cancel_drag(hwnd: HWND) {
    // SAFETY: `cancel_drag` has the same contract as `track_drag`. It does
    // not call `ReleaseCapture` because another component took capture.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        DRAG.with(|c| c.set(None));
        if let Ok(list) = dlg_item(hwnd, drag.section.list) {
            lv_insert_mark(list, None);
        }
    }
}

/// Commits or cancels a drop and releases mouse capture.
///
/// The function cancels the drag when the user releases outside the window.
/// A release outside the list boundaries selects the first or last position
/// in that list.
unsafe fn finish_drag(hwnd: HWND) {
    // SAFETY: `finish_drag` has the same contract as `track_drag`.
    // `ReleaseCapture` has no preconditions, and `released_inside` has
    // its own safety contract.
    unsafe {
        let Some(drag) = drag_of(hwnd) else { return };
        // Clear state before the code releases capture. ReleaseCapture sends
        // WM_CAPTURECHANGED, which would otherwise trigger `cancel_drag`.
        DRAG.with(|c| c.set(None));
        let _ = ReleaseCapture();
        let Ok(list) = dlg_item(hwnd, drag.section.list) else { return };
        lv_insert_mark(list, None);
        // The check returns early when the cursor is outside the window.
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
        // The loop calls `move_selected` for each crossed row. A drag drop
        // and a Move button click share the same reorder logic. Neighbor
        // swaps move the row to `to` and update selection and button state.
        // The dragged row must become the selection before the loop starts.
        lv_select(list, drag.from);
        if lv_selection(list) != drag.from {
            return;
        }
        for _ in 0..(to - drag.from).abs() {
            move_selected(hwnd, drag.section, to < drag.from);
        }
        record_user_edit(hwnd);
    }
}

/// Checks whether the cursor remains within the window rectangle.
///
/// The function checks the full window rectangle. It includes the title bar and frame.
unsafe fn released_inside(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is the valid settings window handle. `rect` and `pt`
    // are local stack storage that outlive each call.
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

/// Returns true when the engine combo selects a plugin.
fn should_show_configure(engine_combo_index: isize) -> bool {
    engine_combo_index > 0
}

fn first_provider_directories(
    entries: impl IntoIterator<Item = (String, PathBuf)>,
) -> HashMap<String, PathBuf> {
    let mut directories = HashMap::new();
    for (name, directory) in entries {
        directories.entry(name).or_insert(directory);
    }
    directories
}

fn windows_hotkey_value(
    config: &crate::config::Config,
    action: crate::config::HotkeyAction,
) -> &str {
    use crate::config::HotkeyAction::*;
    match action {
        Back => "Escape",
        Trigger => &config.trigger.trigger_key,
        AnkiAdd => &config.anki.add_key,
        StaticRegion => &config.anki.static_region_key,
        Screenshot => &config.actions.screenshot.hotkey,
        Search => config.actions.search.hotkey.as_deref().unwrap_or(""),
        SentenceSearch => config.actions.search.sentence_hotkey.as_deref().unwrap_or(""),
        OcrClipboard => config
            .actions
            .ocr_clipboard
            .as_ref()
            .and_then(|action| action.hotkey.as_deref())
            .unwrap_or(""),
    }
}

unsafe fn selected_tab(hwnd: HWND) -> Option<u32> {
    // SAFETY: `ID_TAB` names the live tab control when the window is built.
    unsafe {
        dlg_item(hwnd, ID_TAB)
            .ok()
            .and_then(|tab| u32::try_from(SendMessageW(tab, TCM_GETCURSEL_MSG, None, None).0).ok())
    }
}

/// Updates OCR language availability and configuration button display.
unsafe fn update_engine_controls(hwnd: HWND) {
    // SAFETY: Each identifier names a valid descendant of `hwnd` created
    // in `build`. Absent controls return `Err` and are skipped.
    unsafe {
        let Ok(engine) = dlg_item(hwnd, ID_ENGINE) else {
            return;
        };
        let idx = SendMessageW(engine, CB_GETCURSEL, None, None).0;
        if let Ok(lang) = dlg_item(hwnd, ID_OCR_LANG) {
            let _ = EnableWindow(lang, idx <= 0);
        }
        if let Ok(cfg_btn) = dlg_item(hwnd, ID_ENGINE_CONFIGURE) {
            let on_owner_tab = selected_tab(hwnd) == conditional_tabs(hwnd).engine;
            let cmd = if on_owner_tab && should_show_configure(idx) {
                SW_SHOW
            } else {
                SW_HIDE
            };
            let _ = ShowWindow(cfg_btn, cmd);
        }
    }
}

/// Updates static controls.
unsafe fn update_static_controls(hwnd: HWND) {
    // SAFETY: Each identifier names a valid descendant of `hwnd`
    // created in `build`.
    unsafe {
        let is_static = dlg_item(hwnd, ID_SENTENCE_MODE)
            .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
            .is_ok_and(|i| sentence_mode_at(i) == SentenceMode::Static);
        let selected = selected_tab(hwnd);
        let tabs = conditional_tabs(hwnd);
        let key_visible = selected == tabs.static_key;
        let key_cmd = if key_visible { SW_SHOW } else { SW_HIDE };
        for id in [
            ID_STATIC_REGION_LABEL,
            ID_STATIC_REGION_KEY,
            ID_STATIC_REGION_KEY_CLEAR,
        ] {
            if let Ok(c) = dlg_item(hwnd, id) {
                let _ = ShowWindow(c, key_cmd);
            }
        }
        let overlay_visible = is_static && selected == tabs.static_overlay;
        let overlay_cmd = if overlay_visible { SW_SHOW } else { SW_HIDE };
        for id in [ID_SHOW_STATIC_OVERLAY, ID_STATIC_CAPTURE_HINT] {
            if let Ok(c) = dlg_item(hwnd, id) {
                let _ = ShowWindow(c, overlay_cmd);
            }
        }
    }
}

/// Gets text from an edit control or combo box.
unsafe fn window_text(ctrl: HWND) -> String {
    // SAFETY: `ctrl` is a valid control handle obtained from `dlg_item`.
    // The buffer size matches `GetWindowTextLengthW`, as the
    // `GetWindowTextW` contract requires.
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

unsafe fn combo_row(combo: HWND, index: usize) -> Option<String> {
    // SAFETY: `combo` is a valid combo box handle owned by the caller.
    unsafe {
        let len = SendMessageW(combo, CB_GETLBTEXTLEN, Some(WPARAM(index)), None).0;
        if len < 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = SendMessageW(
            combo,
            CB_GETLBTEXT,
            Some(WPARAM(index)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0;
        if copied < 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..copied as usize]))
    }
}

unsafe fn combo_rows_match(combo: HWND, rows: &[String]) -> bool {
    // SAFETY: `combo` is a valid combo box handle owned by the caller.
    unsafe {
        let count = SendMessageW(combo, CB_GETCOUNT, None, None).0;
        if count < 0 || count as usize != rows.len() {
            return false;
        }
        rows.iter()
            .enumerate()
            .all(|(idx, want)| combo_row(combo, idx).as_ref() == Some(want))
    }
}

unsafe fn fill_combo_if_changed(combo: HWND, rows: &[String]) {
    // SAFETY: `combo` is a valid combo box handle owned by the caller.
    unsafe {
        if combo_rows_match(combo, rows) {
            return;
        }
        let cur = window_text(combo);
        SendMessageW(combo, CB_RESETCONTENT, None, None);
        for name in rows {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(wide(name).as_ptr() as isize)),
            );
        }
        SendMessageW(
            combo,
            WM_SETTEXT,
            None,
            Some(LPARAM(wide(&cur).as_ptr() as isize)),
        );
    }
}

/// Selects `.zip` dictionary archives with a file dialog.
///
/// Returns an empty vector when the user cancels.
unsafe fn pick_archives(owner: HWND) -> Vec<PathBuf> {
    let mut buf = vec![0u16; 32 * 1024];
    // Win32 expects a double null-terminated string.
    let filter: Vec<u16> = "Yomitan archives (*.zip)\0*.zip\0\0"
        .encode_utf16()
        .collect();
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
    // SAFETY: `buf`, `filter`, and `title` outlive the call, and `ofn` borrows
    // them. `nMaxFile` gives the buffer length, so the call cannot overflow it.
    // `lStructSize` gives the structure size for validation.
    let picked = unsafe { GetOpenFileNameW(&mut ofn) }.as_bool();
    if !picked {
        return Vec::new();
    }
    split_picked(&buf)
}

/// Splits the file dialog buffer into path components.
///
/// Supports single-file and multi-file selection formats.
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

/// Returns permitted values for a numeric combo box. It adds `current` when
/// absent.
///
/// The function preserves custom configured values exactly. A custom
/// value like 43 must not change to 45 when the user opens Settings and
/// presses Apply.
fn numeric_choices(lo: i64, hi: i64, step: i64, current: i64) -> Vec<i64> {
    let mut v: Vec<i64> = (lo..=hi).step_by(step as usize).collect();
    // The code clamps out-of-range values as `settings::apply_to` does.
    // Without custom values, the combo box resets to its first entry.
    let current = current.clamp(lo, hi);
    if !v.contains(&current) {
        v.push(current);
        v.sort_unstable();
    }
    v
}


/// Finds the configured source for a field name.
fn default_source<'a>(existing: &'a [crate::config::FieldMapping], field: &str) -> &'a str {
    existing
        .iter()
        .find(|m| m.anki_field == field)
        .map(|m| m.source.as_str())
        .unwrap_or("(none)")
}

fn field_map_chunk_end(next: usize, total: usize) -> usize {
    next.saturating_add(FIELD_MAP_ROWS_PER_PUMP).min(total)
}

fn begin_field_map_result(
    fields: &[String],
    rows: &[(String, HWND)],
    pending: &mut Option<PendingFieldMap>,
) -> bool {
    pending.take();
    !fields.is_empty() && !field_names_match(rows, fields)
}

/// Returns true when rendered rows match model field names.
fn field_names_match(rows: &[(String, HWND)], fields: &[String]) -> bool {
    rows.len() == fields.len() && rows.iter().zip(fields).all(|((n, _), f)| n == f)
}

/// Returns a field map entry, or `None` when the field is unmapped.
fn row_mapping(anki_field: &str, source: &str) -> Option<crate::config::FieldMapping> {
    (source != "(none)").then(|| crate::config::FieldMapping {
        anki_field: anki_field.to_string(),
        source: source.to_string(),
    })
}

/// Merges rendered field rows into the saved field map on Apply.
///
/// `readings` contains one entry per visible row: a field name and selected
/// source. Rows represent note type fields. The configuration keeps mappings
/// for fields that the current model does not show.
///
/// The `"(none)"` sentinel means that a visible field stays unmapped.
/// `row_mapping` discards the sentinel, so the system stores no mapping.
///
/// The function puts visible rows first in model order. It then appends
/// stored mappings in their configuration order.
fn merged_field_map(
    saved: &[crate::config::FieldMapping],
    readings: &[(&str, &str)],
) -> Vec<crate::config::FieldMapping> {
    let mut out: Vec<crate::config::FieldMapping> = readings
        .iter()
        .filter_map(|(field, source)| row_mapping(field, source))
        .collect();
    out.extend(
        saved
            .iter()
            .filter(|m| !readings.iter().any(|(field, _)| *field == m.anki_field))
            .cloned(),
    );
    out
}

/// Returns the number of rows needed in each field-map column.
fn field_map_rows_needed(n: usize) -> i32 {
    n.div_ceil(2).max(1) as i32
}

/// Truncates a label to fit within a field-map column.
fn column_label(name: &str) -> &str {
    name.char_indices()
        .nth(COL_LABEL_MAX_CHARS)
        .map_or(name, |(i, _)| &name[..i])
}

/// Stores display data for one discovered plugin.
struct PluginRow {
    label: String,
    roles: String,
    status: String,
    checked: bool,
    /// The value is false when the plugin is refused.
    can_enable: bool,
}

/// Returns the names of discovered text provider plugins.
fn discovered_text_providers(
    found: &[(PathBuf, Result<crate::plugin::manifest::Manifest>)],
) -> Vec<String> {
    let mut names = Vec::new();
    for (_, parsed) in found {
        let Ok(m) = parsed else {
            continue;
        };
        if m.roles
            .contains(&crate::plugin::manifest::Role::TextProvider)
            && !names.contains(&m.name)
        {
            names.push(m.name.clone());
        }
    }
    names
}

/// Builds display data for one plugin row.
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

/// Returns the directory name for a refused plugin.
fn dir_label(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Returns the plugin name for this row.
fn plugin_key(dir: &Path, parsed: &Result<crate::plugin::manifest::Manifest>) -> String {
    match parsed {
        Ok(m) => m.name.clone(),
        Err(_) => dir_label(dir),
    }
}


/// Formats plugin roles into a comma-separated string.
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

/// Calculates group box height for a specified number of rows.
#[cfg(test)]
fn plugins_group_h(n: usize) -> i32 {
    let body = if n == 0 {
        40
    } else {
        let n = n as i32;
        n * PLUGIN_ROW_H + (n - 1) * ROW_GAP
    };
    20 + body + 8
}

/// Returns the toggle glyph for a collapsed or expanded state.
fn field_map_toggle_label(label: &str, collapsed: bool) -> String {
    format!("{label} {}", if collapsed { '\u{25B6}' } else { '\u{25BC}' })
}

/// Escapes ampersands for Windows control label display.
fn apply_caption(mode: ApplyMode) -> &'static str {
    if mode == ApplyMode::Live {
        "Apply"
    } else {
        "Apply && Restart"
    }
}

/// Returns the Apply hint text.
#[cfg(test)]
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

/// Invalid text leaves the stored value unchanged.
fn parse_px(text: &str, fallback: i32) -> i32 {
    text.trim().parse().unwrap_or(fallback)
}

/// Returns `None` when key capture is not active.
fn search_chord(vk: u16, ctrl: bool, shift: bool, alt: bool, win: bool) -> String {
    let mut parts = Vec::new();
    for (held, name) in [(ctrl, "Ctrl"), (shift, "Shift"), (alt, "Alt"), (win, "Win")] {
        if held { parts.push(name.to_string()); }
    }
    parts.push(match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(u32::from(vk)).unwrap_or('?').to_string(),
        _ => stored_trigger_key(vk),
    });
    parts.join("+")
}

fn set_search_key(hwnd: HWND, id: i32, key: String) {
    let cell = if id == ID_SEARCH_KEY { &SEARCH_CAPTURED } else { &SENTENCE_SEARCH_CAPTURED };
    let text = if key.is_empty() { "Not set" } else { &key };
    // SAFETY: The target is a live capture button; the string is copied synchronously.
    unsafe {
        if let Ok(button) = dlg_item(hwnd, id) {
            let _ = SetWindowTextW(button, PCWSTR(wide(text).as_ptr()));
        }
    }
    cell.with(|cell| *cell.borrow_mut() = Some((hwnd.0 as isize, key)));
}

fn resolved_search_key(hwnd: HWND,
    cell: &'static std::thread::LocalKey<RefCell<Option<(isize, String)>>>,
    template: Option<&str>) -> Option<String> {
    let value = cell.with(|cell| cell.borrow().as_ref()
        .filter(|(owner, _)| *owner == hwnd.0 as isize).map(|(_, key)| key.clone()))
        .or_else(|| template.map(str::to_owned));
    value.filter(|key| !key.is_empty())
}

fn take_captured_key(hwnd: HWND, vk: u16) -> Option<(i32, String)> {
    let mine = hwnd.0 as isize;
    let id = CAPTURING
        .with(|c| c.get())
        .and_then(|(h, id)| (h == mine).then_some(id))?;
    CAPTURING.with(|c| c.set(None));
    let cell = match id {
        ID_TRIGGER_KEY => &CAPTURED_VK,
        ID_STATIC_REGION_KEY => &SR_CAPTURED_VK,
        ID_OCR_CLIPBOARD_KEY => &OCR_CLIP_CAPTURED_VK,
        ID_SCREENSHOT_HOTKEY => &SCREENSHOT_CAPTURED_VK,
        _ => &ANKI_CAPTURED_VK,
    };
    cell.with(|c| c.set(Some((mine, vk))));
    Some((id, crate::config::trigger_key_name(vk)))
}

/// Formats a captured virtual key or returns a template string.
fn resolved_captured_key(
    cell: &'static std::thread::LocalKey<Cell<Option<(isize, u16)>>>,
    hwnd: HWND,
    template: &str,
) -> String {
    cell.with(|c| c.get())
        .and_then(|(h, vk)| (h == hwnd.0 as isize).then_some(vk))
        .or_else(|| crate::config::parse_trigger_key(template))
        .map_or_else(
            || template.to_string(),
            |vk| if vk == 0 { String::new() } else { stored_trigger_key(vk) },
        )
}

/// Returns the hotkey string representation to persist.
fn resolved_trigger_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&CAPTURED_VK, hwnd, template)
}

/// Formats the Anki add hotkey string to persist.
fn resolved_anki_add_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&ANKI_CAPTURED_VK, hwnd, template)
}

/// Formats the static region hotkey string to persist.
fn resolved_sr_key(hwnd: HWND, template: &str) -> String {
    resolved_captured_key(&SR_CAPTURED_VK, hwnd, template)
}

fn resolved_screenshot_key(hwnd: HWND, template: &str) -> String {
    SCREENSHOT_CAPTURED_VK.with(|c| c.get())
        .filter(|(owner, _)| *owner == hwnd.0 as isize)
        .map(|(_, vk)| if vk == 0 { String::new() } else { stored_trigger_key(vk) })
        .unwrap_or_else(|| template.to_string())
}

/// Formats the OCR clipboard hotkey string to persist.
///
/// Converts a "Not set" state to `None`. Internal settings do not use
/// empty strings to indicate disabled state. Refer to ARCHITECTURE.md#settings-and-config.
fn resolved_ocr_clipboard_key(hwnd: HWND, template: Option<&str>) -> Option<String> {
    let key = resolved_captured_key(&OCR_CLIP_CAPTURED_VK, hwnd, template.unwrap_or_default());
    (!key.is_empty()).then_some(key)
}

/// Converts a virtual key code into parseable string format.
fn stored_trigger_key(vk: u16) -> String {
    match vk {
        0x10 => "shift".into(),
        0x11 => "ctrl".into(),
        0x12 => "alt".into(),
        0x70..=0x7B => format!("f{}", vk - 0x6F),
        _ => format!("0x{vk:02X}"),
    }
}

fn measured_text_height(hwnd: HWND, font: Option<HFONT>, text: &str, width: i32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    // SAFETY: The DC belongs to `hwnd`. The selected font remains live.
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if hdc.is_invalid() {
            return ROW_H;
        }
        let old = font.map(|value| SelectObject(hdc, value.into()));
        let mut buffer = wide(text);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: dpi_scale(hwnd, width),
            bottom: 0,
        };
        let height = DrawTextW(
            hdc,
            &mut buffer,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK,
        );
        if let Some(old) = old {
            let _ = SelectObject(hdc, old);
        }
        let _ = ReleaseDC(Some(hwnd), hdc);
        let dpi = window_dpi(hwnd) as i32;
        ((height.max(1) * 96 + dpi - 1) / dpi).max(ROW_H)
    }
}

unsafe extern "system" fn set_child_font(hwnd: HWND, font: LPARAM) -> windows::core::BOOL {
    // SAFETY: EnumChildWindows supplies live descendants; the font stays owned by SettingsWindow.
    unsafe { SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1))); }
    true.into()
}

unsafe fn capture_control_runtime(
    content: HWND,
    entry_top: i32,
    hwnd: HWND,
    font: Option<HFONT>,
) -> ControlRuntime {
    // SAFETY: `hwnd` is a live child of `content`.
    unsafe {
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let mut point = POINT {
            x: rect.left,
            y: rect.top,
        };
        let _ = ScreenToClient(content, &mut point);
        let x = dpi_unscale(content, point.x);
        let y = dpi_unscale(content, point.y) - entry_top;
        let width = dpi_unscale(content, rect.right - rect.left);
        let height = dpi_unscale(content, rect.bottom - rect.top);
        let id = GetDlgCtrlID(hwnd);
        let horizontal = match id {
            ID_SCREENSHOT_SUMMARY => HorizontalLayout::Stretch,
            ID_MODE_LIVE => HorizontalLayout::Quarter(0),
            ID_MODE_HOLD => HorizontalLayout::Quarter(1),
            ID_MODE_TOGGLE => HorizontalLayout::Quarter(2),
            ID_MODE_PRESS => HorizontalLayout::Quarter(3),
            _ if x >= WIN_W - PAD - BTN_W - 16 => HorizontalLayout::MoveRight,
            _ if x + width >= WIN_W - PAD - BTN_W - 24 => HorizontalLayout::Stretch,
            _ => HorizontalLayout::Fixed,
        };
        let mut class = [0u16; 16];
        let class_len = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let class_name = String::from_utf16_lossy(&class[..class_len]);
        let dropdown_height = if class_name.eq_ignore_ascii_case("ComboBox") {
            let mut dropped = RECT::default();
            let result = SendMessageW(hwnd, CB_GETDROPPEDCONTROLRECT, None,
                Some(LPARAM(&mut dropped as *mut _ as isize)));
            (result.0 != 0).then(|| dpi_unscale(content, dropped.bottom - dropped.top))
        } else {
            None
        };
        let text = window_text(hwnd);
        let measured = measured_text_height(content, font, &text, width);
        let wraps = class_name.eq_ignore_ascii_case("Static")
            && (measured - height).abs() <= 1;
        ControlRuntime {
            hwnd,
            x,
            y,
            width,
            height,
            dropdown_height,
            horizontal,
            wraps,
        }
    }
}

pub struct SettingsWindow {
    hwnd: HWND,
    /// Viewport window that clips the content pane.
    viewport: HWND,
    /// Content window that scrolls inside the viewport pane.
    content: HWND,
    font: Cell<Option<HFONT>>,
    font_dpi: Cell<u32>,
    /// Numeric values for each combo box in insertion order. `read` uses
    /// this list to map selection indices back to values.
    widths: Vec<i64>,
    heights: Vec<i64>,
    summaries: Vec<i64>,
    passes: Vec<i64>,
    fonts: Vec<String>,
    /// OCR language tags in combo box order.
    ocr_langs: Vec<String>,
    /// Engine identifiers in combo box order.
    engine_names: Vec<String>,
    /// Map from engine name to plugin directory path.
    engine_dirs: HashMap<String, PathBuf>,
    /// Stores changes that require an Apply update.
    staged: RefCell<SettingsForm>,
    tabs: Vec<TabRuntime>,
    /// Plugin names in checkbox order.
    plugin_names: Vec<String>,
    /// Map from Anki field name to combo box handle.
    field_map_rows: RefCell<Vec<(String, HWND)>>,
    /// Handles for field-map labels and group box.
    field_map_extra: RefCell<Vec<HWND>>,
    pending_field_map: RefCell<Option<PendingFieldMap>>,
    /// True when the field-map section is collapsed.
    field_map_collapsed: Cell<bool>,
    screenshot_summary_height: Cell<i32>,
    /// Maximum bottom vertical coordinate among all tabs.
    bottom_y0: i32,
    /// Index of the active tab.
    current_tab: Cell<u32>,
    /// Operation mode for the Apply button.
    apply_mode: ApplyMode,
    /// True while the settings controls are disabled.
    busy: Cell<bool>,
    apply_state: Cell<ApplyState>,
}

impl SettingsWindow {
    pub fn take_search_request(&self) -> Option<chibipop::search::SearchMode> {
        SEARCH_REQUEST.with(|cell| {
            let (owner, mode) = cell.get()?;
            if owner != self.hwnd.0 as isize { return None; }
            cell.set(None);
            Some(mode)
        })
    }
    /// Creates and displays a settings window from `form`.
    ///
    /// `stale` lists configured dictionary names that are not installed.
    /// The window displays a warning dialog when this list is not empty.
    /// The dialog names those dictionaries.
    ///
    /// `mode` sets the Apply button label and action.
    pub fn open(form: &SettingsForm, stale: &[String], mode: ApplyMode) -> Result<SettingsWindow> {
        let layout = SettingsLayout::embedded()?;
        Self::open_with_layout(form, stale, mode, layout)
    }

    pub(super) fn open_with_layout(
        form: &SettingsForm,
        stale: &[String],
        mode: ApplyMode,
        layout: SettingsLayout,
    ) -> Result<SettingsWindow> {
        layout.validate()?;
        // SAFETY: Window creation FFI calls below use handles owned by this
        // function. Early returns from `?` do not leak resources.
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();
            register_class(hinstance)?;
            register_pane_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name(),
                w!("chibipop settings"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX
                    | WS_THICKFRAME | WS_VSCROLL,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                // Initial placeholder size. `fit_to` adjusts dimensions after build.
                WIN_W,
                400,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for the settings window")?;

            let dpi = GetDpiForWindow(hwnd).max(96);
            WINDOW_DPI.with(|slot| slot.set(Some((hwnd.0 as isize, dpi))));
            let font = ui_font(dpi);
            let mut win = SettingsWindow {
                hwnd,
                // `build` creates the viewport and content panes.
                viewport: HWND::default(),
                content: HWND::default(),
                font: Cell::new(font),
                font_dpi: Cell::new(dpi),
                widths: Vec::new(),
                heights: Vec::new(),
                summaries: Vec::new(),
                passes: Vec::new(),
                fonts: Vec::new(),
                ocr_langs: Vec::new(),
                engine_names: Vec::new(),
                engine_dirs: HashMap::new(),
                staged: RefCell::new(form.clone()),
                tabs: Vec::new(),
                plugin_names: Vec::new(),
                field_map_rows: RefCell::new(Vec::new()),
                field_map_extra: RefCell::new(Vec::new()),
                pending_field_map: RefCell::new(None),
                field_map_collapsed: Cell::new(true),
                screenshot_summary_height: Cell::new(0),
                bottom_y0: 0,
                current_tab: Cell::new(0),
                apply_mode: mode,
                busy: Cell::new(false),
                apply_state: Cell::new(if form.has_staged() {
                    ApplyState::Pending
                } else {
                    ApplyState::Loaded
                }),
            };
            // `build` reports final layout height. The window sizes to
            // match content dimensions. Window frame borders and title bar
            // are accounted for so buttons remain visible across display DPIs.
            let content_h = win.build(form, stale, &layout)?;
            // Populates both sides from a single vector.
            if let Some(tag) = win.selected_language() {
                win.staged.borrow_mut().dict_list_language = tag;
            }
            // Adjusts size and shows the window. Refer to `fit_to` for why
            // `ShowWindow` is not used here.
            win.fit_to(WIN_W, content_h + PAD);
            win.reflow_all_tabs();
            win.resize_content();
            place_bottom(hwnd);
            win.reset_scroll();
            EDIT_TRACKING.with(|slot| slot.set(Some(hwnd.0 as isize)));
            TAB.with(|cell| cell.set(Some((hwnd.0 as isize, 0))));
            win.wake();
            let _ = SetForegroundWindow(hwnd);
            Ok(win)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Activates the current window. It does not open a duplicate.
    ///
    /// Restores minimized windows before activation. `SetForegroundWindow`
    /// does not restore minimized windows.
    pub fn focus(&self) {
        // SAFETY: `self.hwnd` remains valid until `Drop`.
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// Retrieves and clears the queued Apply or Cancel action.
    pub fn take_outcome(&self) -> Option<SettingsOutcome> {
        OUTCOME.with(|c| match c.get() {
            Some((h, o)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(o)
            }
            _ => None,
        })
    }

    /// Retrieves and clears a queued Anki or update click action.
    pub fn take_click(&self) -> Option<SettingsClick> {
        CLICK.with(|c| match c.get() {
            Some((h, k)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(k)
            }
            _ => None,
        })
    }

    /// Takes a live-log request.
    pub fn take_show_logs(&self) -> bool {
        SHOW_LOGS.with(|slot| match slot.get() {
            Some(owner) if owner == self.hwnd.0 as isize => {
                slot.set(None);
                true
            }
            _ => false,
        })
    }

    /// Retrieves text from the Anki URL edit control.
    pub fn anki_url(&self) -> String {
        // SAFETY: `ID_ANKI_URL` is a valid descendant of `self.hwnd`
        // created in `build`.
        unsafe {
            dlg_item(self.hwnd, ID_ANKI_URL)
                .map(|c| window_text(c))
                .unwrap_or_default()
        }
    }

    /// Retrieves text from the Anki model edit control.
    pub fn anki_model(&self) -> String {
        // SAFETY: `ID_ANKI_MODEL` is a valid descendant of `self.hwnd`
        // created in `build`.
        unsafe {
            dlg_item(self.hwnd, ID_ANKI_MODEL)
                .map(|c| window_text(c))
                .unwrap_or_default()
        }
    }

    /// Retrieves the selected theme name from the combo box.
    pub fn read_theme_name(&self) -> String {
        // SAFETY: `ID_THEME` was created in `build` as a valid descendant of `self.hwnd`.
        unsafe {
            let idx = dlg_item(self.hwnd, ID_THEME)
                .map(|c| SendMessageW(c, CB_GETCURSEL, None, None).0)
                .unwrap_or(0);
            if idx == 1 {
                "light".into()
            } else {
                "dark".into()
            }
        }
    }

    /// Retrieves the selected font name from the combo box.
    pub fn read_font_name(&self) -> String {
        // SAFETY: `ID_FONT` was created in `build` as a valid descendant of `self.hwnd`.
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

    /// Processes a queued button action.
    ///
    /// Calls the callback before it opens a file picker.
    pub fn pump(&self, before_blocking: impl FnOnce()) {
        let resized = RESIZED.with(|slot| match slot.get() {
            Some(owner) if owner == self.hwnd.0 as isize => {
                slot.set(None);
                true
            }
            _ => false,
        });
        if resized {
            self.refresh_dpi_font();
            place_bottom(self.hwnd);
            self.reflow_all_tabs();
            self.resize_content();
        }
        let edited = USER_EDIT.with(|slot| slot.get() == Some(self.hwnd.0 as isize));
        if edited && self.apply_state.get() != ApplyState::Applying {
            USER_EDIT.with(|slot| slot.set(None));
            self.set_apply_state(ApplyState::Pending);
        }
        self.pump_field_map();
        if self.take_condition_change() {
            self.reflow_all_tabs();
            // SAFETY: Conditional controls remain live descendants.
            unsafe { update_static_controls(self.hwnd) };
            self.ensure_room_for(self.layout_bottom());
        }
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
        // SAFETY: Each helper operates only on valid descendants of
        // `self.hwnd` that outlive this call.
        unsafe {
            match action {
                Action::Remove(role) => self.remove_selected(role),
                Action::Add => {
                    // Decision D9: The file picker runs an internal message pump.
                    before_blocking();
                    self.add_picked();
                }
                Action::ConfigureEngine => {
                    // Decision D9: The folder picker runs an internal message pump.
                    before_blocking();
                    self.configure_engine();
                }
                Action::ResetScreenshotTargets => self.reset_screenshot_targets(),
            }
        }
    }

    /// Clears the queued Apply operation record.
    pub fn clear_staged(&self) {
        self.staged.borrow_mut().clear_staged();
    }

    /// Updates per-language lists with values that Apply wrote.
    pub fn reseed_per_language(&self, written: &BTreeMap<String, Vec<String>>) {
        self.staged.borrow_mut().reseed_per_language(written);
    }

    /// Displays status text during an Apply operation.
    pub fn set_status(&self, text: &str) {
        // SAFETY: `ID_STATUS` is a valid child of `self.hwnd` created in
        // `build`. `SetWindowTextW` copies the string during the call.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_STATUS) {
                let _ = SetWindowTextW(c, PCWSTR(wide(text).as_ptr()));
            }
        }
    }

    /// Clear both saved screenshot targets in the form and in the controls.
    unsafe fn reset_screenshot_targets(&self) {
        {
            let mut staged = self.staged.borrow_mut();
            staged.cfg.actions.screenshot.fixed_region = None;
            staged.cfg.actions.screenshot.fixed_window = None;
            staged.screenshot_reset_targets = true;
        }
        self.update_screenshot_summary("No saved screenshot targets.");
        // SAFETY: These controls are created by `build` and remain live until
        // this window drops.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_SCREENSHOT_RESET) {
                let _ = EnableWindow(c, false);
            }
            if let Ok(c) = dlg_item(self.hwnd, ID_STATUS) {
                let _ = SetWindowTextW(
                    c,
                    PCWSTR(wide("Saved screenshot targets cleared. Apply to save.").as_ptr()),
                );
            }
        }
    }

    /// Clear the in-memory reset marker after a successful Apply.
    pub fn clear_screenshot_reset_targets(&self) {
        self.staged.borrow_mut().screenshot_reset_targets = false;
    }
    /// Refreshes saved-target controls from the current screenshot configuration.
    ///
    /// An unapplied reset stays visible until the user applies it.
    pub fn refresh_screenshot_targets(
        &self,
        screenshot: &crate::config::ScreenshotConfig,
    ) {
        let (region, window) = {
            let mut staged = self.staged.borrow_mut();
            if staged.screenshot_reset_targets {
                return;
            }
            staged.cfg.actions.screenshot.fixed_region = screenshot.fixed_region;
            staged.cfg.actions.screenshot.fixed_window = screenshot.fixed_window.clone();
            (
                staged.cfg.actions.screenshot.fixed_region,
                staged.cfg.actions.screenshot.fixed_window.clone(),
            )
        };
        self.update_screenshot_summary(&screenshot_target_summary_values(region, window.as_ref()));
        let has_target = !self.busy.get() && (region.is_some() || window.is_some());
        // SAFETY: The reset button belongs to this live settings window.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_SCREENSHOT_RESET) {
                let _ = EnableWindow(c, has_target);
            }
        }
    }

    /// Prevents clipped target text.
    fn update_screenshot_summary(&self, text: &str) {
        let width = logical_client_w(self.hwnd) - 2 * PAD - BTN_W - 28;
        let height = measured_text_height(self.hwnd, self.font.get(), text, width);
        self.screenshot_summary_height.set(height);
        // SAFETY: The summary is a live child of this settings window. Updating
        // its text and dimensions preserves visibility and keyboard order.
        unsafe {
            if let Ok(control) = dlg_item(self.hwnd, ID_SCREENSHOT_SUMMARY) {
                let _ = SetWindowTextW(control, PCWSTR(wide(text).as_ptr()));
                let _ = SetWindowPos(control, None, 0, 0,
                    dpi_scale(self.hwnd, width), dpi_scale(self.hwnd, height),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        if let Some(tab) = self.entry_tab(SettingId::ScreenshotTargets) {
            self.reflow_tab(tab);
        }
        self.ensure_room_for(self.layout_bottom());
    }

    /// Sets the Apply state.
    pub fn set_apply_state(&self, state: ApplyState) {
        let owner = self.hwnd.0 as isize;
        let edited = USER_EDIT.with(|slot| slot.get() == Some(owner));
        let state = if state == ApplyState::Applied && edited {
            ApplyState::Pending
        } else {
            state
        };
        self.apply_state.set(state);
        if state != ApplyState::Pending || edited {
            USER_EDIT.with(|slot| {
                if slot.get() == Some(owner) {
                    slot.set(None);
                }
            });
        }
        let text = format!("Apply: {}", state.label());
        // SAFETY: The footer control remains live until `Drop`.
        unsafe {
            if let Ok(control) = dlg_item(self.hwnd, ID_APPLY_STATE) {
                let _ = SetWindowTextW(control, PCWSTR(wide(&text).as_ptr()));
            }
        }
    }

    /// Sets active runtime status.
    pub fn set_runtime_status(&self, language: &str, engine: &str, anki_enabled: bool) {
        let anki = if anki_enabled { "enabled" } else { "disabled" };
        let text = format!("Language: {language} | OCR: {engine} | Anki: {anki}");
        // SAFETY: The footer control remains live until `Drop`.
        unsafe {
            if let Ok(control) = dlg_item(self.hwnd, ID_RUNTIME_STATUS) {
                let _ = SetWindowTextW(control, PCWSTR(wide(&text).as_ptr()));
            }
        }
    }

    /// Updates controls to show applied dimensions.
    pub fn set_capture_fields(&self, ocr: &crate::config::OcrConfig) {
        // SAFETY: `ID_CAPTURE_W` and `ID_CAPTURE_H` are valid descendants of
        // `self.hwnd` created in `build`. Each `dlg_item` lookup is validated,
        // and `SetWindowTextW` copies text buffers during execution.
        without_edit_tracking(self.hwnd, || {
            // SAFETY: The controls remain live until `Drop`.
            unsafe {
                for (id, px) in [
                    (ID_CAPTURE_W, ocr.capture_width),
                    (ID_CAPTURE_H, ocr.capture_height),
                ] {
                    if let Ok(c) = dlg_item(self.hwnd, id) {
                        let _ = SetWindowTextW(c, PCWSTR(wide(&px.to_string()).as_ptr()));
                    }
                }
            }
        });
    }

    /// Updates Apply button label and status text.
    fn refresh_apply(&self) {
        let staged = self.staged.borrow();
        let has_staged = staged.has_staged();
        // SAFETY: `ID_APPLY` is a live child window.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_APPLY) {
                let caption = wide(apply_caption(self.apply_mode));
                let _ = SetWindowTextW(c, PCWSTR(caption.as_ptr()));
            }
        }
        if has_staged {
            self.set_apply_state(ApplyState::Pending);
        }
    }

    /// Disables controls while Apply runs.
    pub fn set_busy(&self, busy: bool) {
        // SAFETY: Each identifier in `WHILE_BUSY` names a valid descendant of
        // `self.hwnd` created in `build`. Focus moves off the controls first
        // so keyboard input is not trapped on disabled controls.
        self.busy.set(busy);
        unsafe {
            if busy {
                let _ = SetFocus(Some(self.hwnd));
            }
            for id in WHILE_BUSY {
                if let Ok(c) = dlg_item(self.hwnd, id) {
                    let enabled = if id == ID_SCREENSHOT_RESET && !busy {
                        let staged = self.staged.borrow();
                        !staged.screenshot_reset_targets
                            && (staged.cfg.actions.screenshot.fixed_region.is_some()
                                || staged.cfg.actions.screenshot.fixed_window.is_some())
                    } else {
                        !busy
                    };
                    let _ = EnableWindow(c, enabled);
                }
            }
            if !busy {
                update_list_buttons(self.hwnd);
                update_engine_controls(self.hwnd);
            }
        }
    }

    /// Retrieves and clears a queued tab switch index.
    pub fn take_tab_change(&self) -> Option<u32> {
        TAB.with(|c| match c.get() {
            Some((h, tab)) if h == self.hwnd.0 as isize => {
                c.set(None);
                Some(tab)
            }
            _ => None,
        })
    }

    /// Retrieves and clears a queued OCR language switch index.
    fn take_language_change(&self) -> bool {
        LANG_CHANGED.with(|c| match c.get() {
            Some(h) if h == self.hwnd.0 as isize => {
                c.set(None);
                true
            }
            _ => false,
        })
    }

    /// Retrieves the selected OCR language tag from the combo box.
    fn selected_language(&self) -> Option<String> {
        // SAFETY: `ID_OCR_LANG` is a valid descendant of `self.hwnd` created
        // in `build`. Absent controls return `Err`.
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

    /// Returns the plugin directory path for the selected OCR engine.
    fn selected_engine_dir(&self) -> Option<&Path> {
        // SAFETY: `ID_ENGINE` is a valid descendant of `self.hwnd`
        // created in `build`.
        let idx = unsafe {
            let Ok(e) = dlg_item(self.hwnd, ID_ENGINE) else {
                return None;
            };
            SendMessageW(e, CB_GETCURSEL, None, None).0 as usize
        };
        let name = self.engine_names.get(idx)?;
        self.engine_dirs.get(name).map(|p| p.as_path())
    }

    /// Returns the selected OCR engine name.
    fn selected_engine_name(&self) -> Option<&str> {
        // SAFETY: `ID_ENGINE` is a valid descendant of `self.hwnd`
        // created in `build`.
        let idx = unsafe {
            let Ok(e) = dlg_item(self.hwnd, ID_ENGINE) else {
                return None;
            };
            SendMessageW(e, CB_GETCURSEL, None, None).0 as usize
        };
        self.engine_names.get(idx).map(|s| s.as_str())
    }

    /// Updates dictionary rows for a new OCR language.
    ///
    /// Keeps the previous selection when the new language supports it.
    fn rescope_dicts(&self) {
        let Some(next) = self.selected_language() else {
            return;
        };
        let mut staged = self.staged.borrow_mut();
        let prev = staged.dict_list_language.clone();
        if prev == next {
            return;
        }
        // SAFETY: `ID_TERMS` is a valid descendant of `self.hwnd` created
        // in `build`. `lv_rows` and `fill_role_list` meet their conditions.
        unsafe {
            let Some(rows) = lv_rows(self.hwnd, ID_TERMS) else { return };
            staged.terms = rows;
            staged.cfg.ocr.language = prev.clone();
            if crate::settings::is_scoped(&staged) {
                if let Some(keys) = crate::settings::scoped_entry(
                    &staged.terms, &staged.unreadable) {
                    staged.cfg.dictionaries.per_language.insert(prev, keys);
                }
            }
            let all: Vec<String> = staged.terms.iter().map(|row| row.name.clone()).collect();
            let list = staged.cfg.dictionaries.per_language.get(&next).cloned().unwrap_or_default();
            let scoped = scope_rows(&all, &list, &staged.unreadable);
            staged.terms = scoped;
            staged.dict_list_language = next.clone();
            staged.cfg.ocr.language = next;
            if let Ok(terms) = dlg_item(self.hwnd, ID_TERMS) {
                fill_role_list(terms, &staged.terms, 0);
            }
            update_list_buttons(self.hwnd);
        }
    }

    /// Checks and clears the queued field-map toggle flag.
    pub fn take_field_map_toggle(&self) -> bool {
        FIELD_MAP_TOGGLE.with(|c| match c.get() {
            Some(h) if h == self.hwnd.0 as isize => {
                c.set(None);
                true
            }
            _ => false,
        })
    }

    /// Retrieves and clears a queued Anki model switch selection.
    pub fn take_anki_model_change(&self) -> bool {
        ANKI_MODEL_CHANGED.with(|c| match c.get() {
            Some(h) if h == self.hwnd.0 as isize => {
                c.set(None);
                true
            }
            _ => false,
        })
    }

    pub fn tab_count(&self) -> u32 {
        u32::try_from(self.tabs.len()).unwrap_or(u32::MAX)
    }

    pub fn tab_label(&self, index: u32) -> Option<&str> {
        self.tabs.get(index as usize).map(|tab| tab.label.as_str())
    }

    pub(super) fn tab_id(&self, index: u32) -> Option<TabId> {
        self.tabs.get(index as usize).map(|tab| tab.id)
    }

    pub fn field_map_tab(&self) -> Option<u32> {
        self.entry_tab(SettingId::AnkiFieldMap)
    }

    pub fn tab_needs_anki_detection(&self, index: u32) -> bool {
        self.tabs.get(index as usize).is_some_and(|tab| {
            [
                SettingId::AnkiDeck,
                SettingId::AnkiModel,
                SettingId::AnkiRefresh,
                SettingId::AnkiFieldMap,
            ]
            .iter()
            .any(|&id| tab.sections.iter().any(|section| {
                section.entries.iter().any(|entry| entry.id == id)
            }))
        })
    }

    fn entry_tab(&self, id: SettingId) -> Option<u32> {
        self.tabs.iter().enumerate().find_map(|(tab_index, tab)| {
            tab.sections
                .iter()
                .any(|section| section.entries.iter().any(|entry| entry.id == id))
                .then_some(tab_index as u32)
        })
    }

    #[cfg(test)]
    fn entry_top(&self, id: SettingId) -> Option<i32> {
        self.tabs.iter().find_map(|tab| {
            tab.sections.iter().find_map(|section| {
                section
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.top.get())
            })
        })
    }

    fn entry_label(&self, id: SettingId) -> Option<&str> {
        self.tabs.iter().find_map(|tab| {
            tab.sections.iter().find_map(|section| {
                section
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.label.as_str())
            })
        })
    }

    fn sentence_is_static(&self) -> bool {
        // SAFETY: The sentence combo remains live until `Drop`.
        unsafe {
            dlg_item(self.hwnd, ID_SENTENCE_MODE)
                .map(|control| SendMessageW(control, CB_GETCURSEL, None, None).0)
                .is_ok_and(|index| sentence_mode_at(index) == SentenceMode::Static)
        }
    }

    pub fn validate_hotkeys(&self, pending: &crate::config::Config) -> Result<()> {
        let error = match pending.validate_hotkeys(crate::config::Platform::Windows) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if let Some(&(first, second)) = pending
            .hotkey_conflicts(crate::config::Platform::Windows)
            .first()
        {
            let conflict = format!("{} conflicts with {}", second.name(), first.name());
            if error.to_string().starts_with(&conflict) {
                anyhow::bail!(
                    "{} ({}) conflicts with {} ({}). Choose different keys.",
                    second.name(),
                    windows_hotkey_value(pending, second),
                    first.name(),
                    windows_hotkey_value(pending, first),
                );
            }
        }
        Err(error)
    }

    fn field_map_entry_height(&self, base_height: i32) -> i32 {
        let count = self.pending_field_map.borrow().as_ref().map_or_else(
            || self.field_map_rows.borrow().len(),
            |pending| pending.fields.len(),
        );
        if count == 0 || self.field_map_collapsed.get() {
            base_height
        } else {
            base_height + 20 + field_map_rows_needed(count) * ROW_H + 8
        }
    }

    fn entry_height(&self, entry: &EntryRuntime) -> i32 {
        match entry.id {
            SettingId::AnkiFieldMap => self.field_map_entry_height(entry.base_height.get()),
            SettingId::AnkiStaticOverlay if !self.sentence_is_static() => 0,
            _ => entry.base_height.get(),
        }
    }

    fn reflow_entry(&self, entry: &EntryRuntime, width: i32) {
        let delta_w = width - WIN_W;
        let mut rows: Vec<i32> = entry.controls.iter().map(|control| control.y).collect();
        rows.sort_unstable();
        rows.dedup();
        let mut delta_y = 0;
        for row in rows {
            let mut row_delta = None;
            for control in entry.controls.iter().filter(|control| control.y == row) {
                let (x, control_w) = match control.horizontal {
                    HorizontalLayout::Fixed => (control.x, control.width),
                    HorizontalLayout::Stretch => {
                        (control.x, (control.width + delta_w).max(40))
                    }
                    HorizontalLayout::MoveRight => (control.x + delta_w, control.width),
                    HorizontalLayout::Quarter(index) => {
                        let area = width - 2 * PAD - 20;
                        let gap = 8;
                        let item_w = (area - 3 * gap) / 4;
                        (PAD + i32::from(index) * (item_w + gap), item_w)
                    }
                };
                let height = if control.wraps {
                    // SAFETY: The control remains live until `Drop`.
                    let text = unsafe { window_text(control.hwnd) };
                    measured_text_height(self.hwnd, self.font.get(), &text, control_w)
                } else {
                    control.height
                };
                let height_delta = height - control.height;
                row_delta = Some(row_delta.map_or(height_delta, |delta: i32| {
                    delta.max(height_delta)
                }));
                // SAFETY: The control is a live child of the content pane.
                unsafe {
                    let _ = SetWindowPos(
                        control.hwnd,
                        None,
                        dpi_scale(self.hwnd, x),
                        dpi_scale(self.hwnd, entry.top.get() + control.y + delta_y),
                        dpi_scale(self.hwnd, control_w),
                        dpi_scale(self.hwnd, control.dropdown_height.unwrap_or(height)),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let id = GetDlgCtrlID(control.hwnd);
                    if [ID_TERMS, ID_FREQS, ID_PITCH].contains(&id) {
                        SendMessageW(
                            control.hwnd,
                            LVM_SETCOLUMNWIDTH,
                            Some(WPARAM(0)),
                            Some(LPARAM(LVSCW_AUTOSIZE_USEHEADER as isize)),
                        );
                    }
                }
            }
            delta_y += row_delta.unwrap_or(0);
        }
        entry.base_height.set((entry.initial_height + delta_y).max(ROW_H));
    }

    fn reflow_tab(&self, tab_index: u32) {
        let Some(tab) = self.tabs.get(tab_index as usize) else { return };
        let width = logical_client_w(self.hwnd);
        let mut y = 0;
        for section in &tab.sections {
            let section_top = y;
            section.top.set(section_top);
            y += 20;
            for entry in &section.entries {
                entry.top.set(y);
                self.reflow_entry(entry, width);
                if entry.id == SettingId::AnkiFieldMap {
                    self.repack_field_map();
                }
                y += self.entry_height(entry);
            }
            let height = (y - section_top + 8).max(28);
            section.height.set(height);
            // SAFETY: The frame remains a live child until `Drop`.
            unsafe {
                let _ = SetWindowPos(
                    section.frame,
                    None,
                    dpi_scale(self.hwnd, PAD - 6),
                    dpi_scale(self.hwnd, section_top),
                    dpi_scale(self.hwnd, width - 2 * PAD),
                    dpi_scale(self.hwnd, height),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            y += GROUP_GAP;
        }
        tab.page_height.set(y);
    }

    fn reflow_all_tabs(&self) {
        for index in 0..self.tab_count() {
            self.reflow_tab(index);
        }
    }

    fn reorder_tab_z_order(&self, tab_index: u32) {
        let Some(tab) = self.tabs.get(tab_index as usize) else { return };
        let mut handles = Vec::new();
        for section in &tab.sections {
            handles.push(section.frame);
            for entry in &section.entries {
                handles.extend(entry.controls.iter().map(|control| control.hwnd));
                if entry.id == SettingId::AnkiFieldMap {
                    handles.extend(self.field_map_extra.borrow().iter().copied());
                    handles.extend(
                        self.field_map_rows
                            .borrow()
                            .iter()
                            .map(|(_, control)| *control),
                    );
                }
            }
        }
        // SAFETY: Every handle is a live child of the content pane.
        unsafe {
            let mut after = HWND_TOP;
            for control in handles {
                let _ = SetWindowPos(
                    control,
                    Some(after),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                after = control;
            }
        }
    }

    fn layout_bottom(&self) -> i32 {
        CONTENT_Y
            + self
                .tabs
                .iter()
                .map(|tab| tab.page_height.get())
                .max()
                .unwrap_or(0)
    }

    fn take_condition_change(&self) -> bool {
        CONDITION_CHANGED.with(|cell| match cell.get() {
            Some(owner) if owner == self.hwnd.0 as isize => {
                cell.set(None);
                true
            }
            _ => false,
        })
    }

    /// Tab page height in page coordinates.
    ///
    /// Returns the page height.
    fn tab_page_h(&self, tab: u32) -> i32 {
        self.tabs
            .get(tab as usize)
            .map_or(0, |runtime| runtime.page_height.get())
    }

    /// Recalculates scroll range for the active tab and scrolls to top.
    fn reset_scroll(&self) {
        let content_h = dpi_scale(self.hwnd, self.tab_page_h(self.current_tab.get()));
        set_scroll_range(self.hwnd, content_h, client_h(self.viewport), 0);
    }

    fn refresh_dpi_font(&self) {
        let dpi = window_dpi(self.hwnd);
        if self.font_dpi.get() == dpi {
            return;
        }
        // SAFETY: The new font stays owned until every descendant switches away from it or is destroyed.
        unsafe {
            if let Some(font) = ui_font(dpi) {
                let _ = EnumChildWindows(Some(self.hwnd), Some(set_child_font), LPARAM(font.0 as isize));
                if let Some(previous) = self.font.replace(Some(font)) {
                    let _ = DeleteObject(previous.into());
                }
                self.font_dpi.set(dpi);
            }
        }
    }

    fn keep_focus_visible(&self) {
        // SAFETY: The focused descendant and both panes remain live during this owner-thread call.
        unsafe {
            let focus = GetFocus();
            if !IsChild(self.content, focus).as_bool() || !IsWindowVisible(focus).as_bool() {
                return;
            }
            let mut rect = RECT::default();
            if GetWindowRect(focus, &mut rect).is_err() {
                return;
            }
            let mut origin = POINT { x: rect.left, y: rect.top };
            if !ScreenToClient(self.content, &mut origin).as_bool() {
                return;
            }
            let height = rect.bottom - rect.top;
            scroll_to(self.hwnd, |info| {
                let page = info.nPage as i32;
                if origin.y < info.nPos || height >= page {
                    origin.y
                } else if origin.y + height > info.nPos + page {
                    origin.y + height - page
                } else {
                    info.nPos
                }
            });
        }
    }

    /// Shows the selected tab and hides all other tabs.
    pub fn switch_tab(&self, tab: u32) {
        // SAFETY: `self.hwnd` remains valid until `Drop`.
        unsafe { cancel_capture(self.hwnd) };
        if tab as usize >= self.tabs.len() {
            return;
        }
        self.current_tab.set(tab);
        // SAFETY: Runtime handles remain live descendants until `Drop`.
        unsafe {
            if let Ok(control) = dlg_item(self.hwnd, ID_TAB) {
                SendMessageW(control, TCM_SETCURSEL_MSG, Some(WPARAM(tab as usize)), None);
            }
            for (i, runtime) in self.tabs.iter().enumerate() {
                let cmd = if i as u32 == tab { SW_SHOW } else { SW_HIDE };
                for section in &runtime.sections {
                    let _ = ShowWindow(section.frame, cmd);
                    for entry in &section.entries {
                        for control in &entry.controls {
                            let _ = ShowWindow(control.hwnd, cmd);
                        }
                    }
                }
            }
            self.apply_field_map_visibility();
            update_engine_controls(self.hwnd);
            update_static_controls(self.hwnd);
        }
        self.reset_scroll();
    }

    /// Toggles field-map collapse state and resizes the window.
    pub fn toggle_field_map(&self) {
        let collapsed = !self.field_map_collapsed.get();
        self.field_map_collapsed.set(collapsed);
        // SAFETY: `self.hwnd` remains valid until `Drop`. `ID_FIELD_MAP_TOGGLE`
        // and controls touched by `apply_field_map_visibility` are child windows.
        unsafe {
            self.apply_field_map_visibility();
            if let Ok(btn) = dlg_item(self.hwnd, ID_FIELD_MAP_TOGGLE) {
                let label = self.entry_label(SettingId::AnkiFieldMap).unwrap_or("Field mapping");
                let text = field_map_toggle_label(label, collapsed);
                let _ = SetWindowTextW(btn, PCWSTR(wide(&text).as_ptr()));
            }
        }
        if let Some(tab) = self.field_map_tab() {
            self.reflow_tab(tab);
            self.reorder_tab_z_order(tab);
        }
        self.ensure_room_for(self.layout_bottom());
    }

    /// Records captured key code `vk`. Returns true if accepted.
    pub fn handle_capture_key(&self, vk: u16) -> bool {
        let screenshot = CAPTURING.with(|c| c.get())
            == Some((self.hwnd.0 as isize, ID_SCREENSHOT_HOTKEY));
        let capturing = CAPTURING.with(|c| {
            c.get().is_some_and(|(owner, _)| owner == self.hwnd.0 as isize)
        });
        if capturing && vk == 0x1B {
            // SAFETY: Capture belongs to this live settings window.
            unsafe { cancel_capture(self.hwnd); }
            return true;
        }
        let search = CAPTURING.with(|c| c.get()).filter(|(owner, id)|
            *owner == self.hwnd.0 as isize && matches!(*id, ID_SEARCH_KEY | ID_SENTENCE_SEARCH_KEY));
        if let Some((_, id)) = search {
            if matches!(vk, 0x10..=0x12 | 0x5B..=0x5C | 0xA0..=0xA5) { return true; }
            // SAFETY: GetKeyState reads the current UI thread's modifier state.
            let key = unsafe { search_chord(vk,
                windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11) < 0,
                windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) < 0,
                windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x12) < 0,
                windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x5B) < 0
                    || windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x5C) < 0) };
            if crate::config::parse_hotkey(&key).is_none() {
                self.set_status("Use a key with Ctrl, Shift, or Alt. Escape cancels capture.");
                return true;
            }
            CAPTURING.with(|c| c.set(None));
            set_search_key(self.hwnd, id, key);
            record_user_edit(self.hwnd);
            return true;
        }
        if screenshot && matches!(vk, 0x10..=0x12 | 0xA0..=0xA5) {
            return true;
        }
        let Some((id, text)) = take_captured_key(self.hwnd, vk) else {
            return false;
        };
        // SAFETY: `id` is a key capture button identifier and a valid
        // descendant of `self.hwnd` created in `build`. `SetWindowTextW`
        // copies the text string during the call.
        unsafe {
            if let Ok(btn) = dlg_item(self.hwnd, id) {
                let _ = SetWindowTextW(btn, PCWSTR(wide(&text).as_ptr()));
            }
        }
        record_user_edit(self.hwnd);
        true
    }

    /// Populates Anki deck and model combos and field-map rows.
    pub fn populate_combos(&self, decks: &[String], models: &[String], fields: Vec<String>) {
        // SAFETY: `ID_ANKI_DECK` and `ID_ANKI_MODEL` are valid descendants of
        // `self.hwnd` created in `build`. `SendMessageW` copies text buffers.
        without_edit_tracking(self.hwnd, || {
            // SAFETY: The controls remain live until `Drop`.
            unsafe {
                if let Ok(deck) = dlg_item(self.hwnd, ID_ANKI_DECK) {
                    fill_combo_if_changed(deck, decks);
                }
                if let Ok(model) = dlg_item(self.hwnd, ID_ANKI_MODEL) {
                    fill_combo_if_changed(model, models);
                }
            }
        });
        self.populate_field_map(fields);
    }

    /// Rebuilds field-map rows without updates to the deck or model combos.
    pub fn populate_fields(&self, fields: Vec<String>) {
        self.populate_field_map(fields);
    }

    /// Rebuilds field-map rows for the current Anki note type.
    ///
    /// Returns without changes when field names are empty or unchanged.
    ///
    /// Creates one row for each note type field. Saved mappings for absent
    /// fields have no row. `merged_field_map` keeps those mappings during save.
    /// Set a field row to `"(none)"` to remove its mapping.
    fn populate_field_map(&self, fields: Vec<String>) {
        let (needs_rebuild, cancelled_pending) = {
            let rows = self.field_map_rows.borrow();
            let mut pending = self.pending_field_map.borrow_mut();
            let cancelled_pending = pending.is_some();
            (begin_field_map_result(&fields, &rows, &mut pending), cancelled_pending)
        };
        if !needs_rebuild {
            if cancelled_pending {
                self.repack_field_map();
                if let Some(tab) = self.field_map_tab() {
                    self.reflow_tab(tab);
                }
                self.ensure_room_for(self.layout_bottom());
            }
            return;
        }
        // SAFETY: Every window handle in `field_map_extra` and `field_map_rows`
        // was created as a descendant of `self.hwnd` and is destroyed here.
        unsafe {
            for hwnd in self.field_map_extra.borrow_mut().drain(..) {
                let _ = DestroyWindow(hwnd);
            }
            for (_, hwnd) in self.field_map_rows.borrow_mut().drain(..) {
                let _ = DestroyWindow(hwnd);
            }
        }
        // Unmapped fields default to `"(none)"` through `default_source`.
        let existing = self.staged.borrow().field_map.clone().unwrap_or_default();
        if let Some(group) = self.build_field_map_box(fields.len()) {
            self.field_map_extra.borrow_mut().push(group);
        }
        *self.pending_field_map.borrow_mut() = Some(PendingFieldMap::new(fields, existing));
        self.pump_field_map();
    }

    fn repack_field_map(&self) {
        let rows = self.field_map_rows.borrow();
        let extra = self.field_map_extra.borrow();
        let label_start = usize::from(extra.len() == rows.len() + 1);
        let rows_n = field_map_rows_needed(rows.len());
        let y0 = self.field_map_dynamic_top();
        let area_w = logical_client_w(self.hwnd) - 2 * PAD - 20;
        let col_w = (area_w - COL_GAP) / 2;
        let label_w = COL_LABEL_W.min(col_w / 2);
        let combo_w = (col_w - label_w - COL_LABEL_GAP).max(80);
        // SAFETY: The vectors own live controls. Each row has one extra label,
        // preceded by the optional group frame. Moving preserves values and focus.
        unsafe {
            if label_start == 1 {
                let height = if rows.is_empty() { 0 } else { 20 + rows_n * ROW_H + 8 };
                let _ = SetWindowPos(extra[0], None,
                    dpi_scale(self.hwnd, PAD - 6), dpi_scale(self.hwnd, y0),
                    dpi_scale(self.hwnd, logical_client_w(self.hwnd) - 2 * PAD),
                    dpi_scale(self.hwnd, height),
                    SWP_NOZORDER | SWP_NOACTIVATE);
            }
            for (index, (_, combo)) in rows.iter().enumerate() {
                let Ok(index_i32) = i32::try_from(index) else { break };
                let x = PAD + index_i32 / rows_n * (col_w + COL_GAP);
                let y = y0 + 20 + index_i32 % rows_n * ROW_H;
                if let Some(&label) = extra.get(index + label_start) {
                    let _ = SetWindowPos(label, None,
                        dpi_scale(self.hwnd, x), dpi_scale(self.hwnd, y + 4),
                        dpi_scale(self.hwnd, label_w), dpi_scale(self.hwnd, ROW_H),
                        SWP_NOZORDER | SWP_NOACTIVATE);
                }
                let _ = SetWindowPos(*combo, None,
                    dpi_scale(self.hwnd, x + label_w + COL_LABEL_GAP),
                    dpi_scale(self.hwnd, y), dpi_scale(self.hwnd, combo_w),
                    dpi_scale(self.hwnd, 140), SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }

    fn pump_field_map(&self) {
        let has_more = {
            let mut pending = self.pending_field_map.borrow_mut();
            let Some(build) = pending.as_mut() else {
                return;
            };
            let total = build.fields.len();
            let end = field_map_chunk_end(build.next, build.fields.len());
            for idx in build.next..end {
                let name = &build.fields[idx];
                if let Some((label, row)) =
                    self.build_field_map_row(total, idx, name, &build.existing)
                {
                    self.field_map_extra.borrow_mut().push(label);
                    self.field_map_rows.borrow_mut().push(row);
                }
            }
            build.next = end;
            let has_more = build.next < build.fields.len();
            if !has_more {
                pending.take();
            }
            has_more
        };
        // SAFETY: Each window handle was created as a valid descendant of `self.hwnd`.
        unsafe { self.apply_field_map_visibility() };
        if let Some(tab) = self.field_map_tab() {
            self.reflow_tab(tab);
            self.reorder_tab_z_order(tab);
        }
        self.ensure_room_for(self.layout_bottom());
        if has_more {
            self.wake();
        }
    }

    fn wake(&self) {
        // SAFETY: `self.hwnd` remains valid until `Drop`. WM_NULL wakes the message pump.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
    /// Shows or hides field-map rows based on collapse state.
    unsafe fn apply_field_map_visibility(&self) {
        let visible = self.field_map_tab() == Some(self.current_tab.get())
            && !self.field_map_collapsed.get();
        let cmd = if visible { SW_SHOW } else { SW_HIDE };
        // SAFETY: Each window handle here is a valid descendant of `self.hwnd`
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

    fn field_map_dynamic_top(&self) -> i32 {
        self.tabs
            .iter()
            .flat_map(|tab| &tab.sections)
            .flat_map(|section| &section.entries)
            .find(|entry| entry.id == SettingId::AnkiFieldMap)
            .map_or(0, |entry| entry.top.get() + entry.base_height.get())
    }

    /// Places the viewport immediately below the tab strip in z-order.
    ///
    /// Tab navigation follows z-order, so pages must precede the Apply row.
    /// Only child windows of the main window affect this placement.
    unsafe fn place_viewport(&self) {
        // SAFETY: `self.viewport` is a valid child of `self.hwnd` created in
        // `build`. `GetDlgItem` returns the tab control, which is a sibling
        // window as required by `SetWindowPos`. `SWP_NOSIZE` and `SWP_NOMOVE`
        // preserve window dimensions and coordinates.
        unsafe {
            let Ok(after) = GetDlgItem(Some(self.hwnd), ID_TAB) else {
                return;
            };
            let _ = SetWindowPos(
                self.viewport,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn build_field_map_box(&self, field_count: usize) -> Option<HWND> {
        let f = self.font.get();
        let page = self.content;
        let y0 = self.field_map_dynamic_top();
        let rows_n = field_map_rows_needed(field_count);
        let map_h = 20 + rows_n * ROW_H + 8;
        // SAFETY: `h` is `self.hwnd` and `page` is its content pane.
        // Both windows are valid during the call. Created controls are
        // children of `page` and outlive this function.
        unsafe {
            child(
                page,
                w!("BUTTON"),
                "",
                WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
                PAD - 6,
                y0,
                WIN_W - 2 * PAD,
                map_h,
                0,
                f,
            )
            .ok()
        }
    }

    fn build_field_map_row(
        &self,
        total: usize,
        idx: usize,
        name: &str,
        existing: &[crate::config::FieldMapping],
    ) -> Option<(HWND, (String, HWND))> {
        let f = self.font.get();
        let h = self.hwnd;
        let page = self.content;
        let y0 = self.field_map_dynamic_top();
        let rows_n = field_map_rows_needed(total);
        let idx_i32 = i32::try_from(idx).ok()?;
        let col = idx_i32 / rows_n;
        let row = idx_i32 % rows_n;
        let x = PAD + col * (COL_W + COL_GAP);
        let y = y0 + 20 + row * ROW_H;
        // SAFETY: `h` and `page` are valid windows owned by this instance.
        unsafe {
            let label = child(
                page,
                w!("STATIC"),
                column_label(name),
                WINDOW_STYLE(0),
                x,
                y + 4,
                COL_LABEL_W,
                ROW_H,
                0,
                f,
            )
            .ok()?;
            let combo_x = x + COL_LABEL_W + COL_LABEL_GAP;
            let combo = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                combo_x,
                y,
                COL_COMBO_W,
                140,
                ID_FIELD_MAP_BASE + idx_i32,
                f,
            )
            .ok()?;
            let want = default_source(existing, name);
            for (j, src) in FIELD_MAP_SOURCES.iter().enumerate() {
                SendMessageW(
                    combo,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(src).as_ptr() as isize)),
                );
                if *src == want {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(j)), None);
                }
            }
            if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            SendMessageW(
                combo,
                CB_SETDROPPEDWIDTH,
                Some(WPARAM(dpi_scale(h, COL_DROPPED_W) as usize)),
                None,
            );
            Some((label, (name.to_string(), combo)))
        }
    }

    /// Updates the scrollable page.
    fn ensure_room_for(&self, _needed_bottom: i32) {
        self.resize_content();
    }

    fn resize_content(&self) {
        let height = (self.layout_bottom() - CONTENT_Y).max(1);
        let position = scroll_position(self.hwnd);
        // SAFETY: `self.content` remains live until `Drop`.
        unsafe {
            let _ = SetWindowPos(
                self.content,
                None,
                0,
                0,
                client_w(self.hwnd),
                dpi_scale(self.hwnd, height),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let content_h = dpi_scale(self.hwnd, self.tab_page_h(self.current_tab.get()));
        set_scroll_range(self.hwnd, content_h, client_h(self.viewport), position);
        self.keep_focus_visible();
    }

    /// Removes the selected dictionary row from all role sections.
    ///
    /// An archive represents one library entry. The remove action removes its
    /// name from all three role lists. `role` identifies the section that
    /// supplied the selection. Refer to ARCHITECTURE.md#dictionary-and-lookup.
    unsafe fn remove_selected(&self, role: Role) {
        // SAFETY: Each `section.list` identifies a valid descendant of
        // `self.hwnd` created in `build`. Absent controls return `Err`.
        // `lv_*` helpers and `update_list_buttons` meet their safety conditions.
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
                // Selects the row below the deleted item, or clears selection when empty.
                lv_select(list, at.min(lv_count(list) - 1));
            }
            self.staged.borrow_mut().stage_remove(&name);
            update_list_buttons(self.hwnd);
            self.refresh_apply();
        }
    }

    /// Stages selected dictionary archives for addition.
    unsafe fn add_picked(&self) {
        // SAFETY: `pick_archives` owns all dialog buffers. Each `section.list`
        // identifies a valid descendant of `self.hwnd`. `lv_append` and
        // `lv_select` meet their safety conditions.
        unsafe {
            let picked = pick_archives(self.hwnd);
            for path in picked {
                // Archive roles determine destination lists. An archive can
                // add items to multiple lists.
                let Some(roles) = self.staged.borrow_mut().stage_add(&path) else {
                    eprintln!(
                        "chibipop: {} is already listed, or is not a dictionary chibipop can read.",
                        path.display()
                    );
                    continue;
                };
                let Some(name) = self
                    .staged
                    .borrow()
                    .staged_adds
                    .last()
                    .map(|a| a.name.clone())
                else {
                    continue;
                };
                // Appends items selected to the bottom of each role list.
                // Current rows preserve their order.
                // Refer to ARCHITECTURE.md#dictionary-and-lookup.
                let row = DictRow { name, enabled: true };
                for section in SECTIONS.iter().filter(|s| roles.has(s.role)) {
                    let Ok(list) = dlg_item(self.hwnd, section.list) else { continue };
                    let at = lv_append(list, &row);
                    // Scrolls to the new item so imported rows remain visible.
                    lv_select(list, at);
                }
            }
            update_list_buttons(self.hwnd);
            self.refresh_apply();
        }
    }

    /// Selects a folder with a dialog and saves the path.
    unsafe fn configure_engine(&self) {
        let Some(name) = self.selected_engine_name() else {
            return;
        };
        let Some(dir) = self.selected_engine_dir() else {
            return;
        };
        let title = format!("Select your {name} installation");
        // SAFETY: `self.hwnd` is a valid window handle. The folder picker
        // frees its PIDL allocation before return.
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

    /// Resizes the client area to `client_w` by `client_h` 96-DPI pixels,
    /// and displays the window.
    ///
    /// Frame metrics and the native scrollbar reserve space at the window DPI.
    fn fit_to(&self, client_w: i32, client_h: i32) {
        // SAFETY: `self.hwnd` is a valid window handle. `rc` is local stack
        // storage that the call modifies. Failure leaves the current window size.
        unsafe {
            if let Ok(size) = outer_size_for_client(self.hwnd,
                dpi_scale(self.hwnd, client_w), dpi_scale(self.hwnd, client_h),
                window_dpi(self.hwnd)) {
                let mut outer_h = size.y;
                if let Some(cap) = work_area_height(self.hwnd) {
                    outer_h = outer_h.min(cap);
                }
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    size.x,
                    outer_h,
                    // Use SWP_SHOWWINDOW instead of a separate `ShowWindow` call.
                    // The first `ShowWindow` call in a process uses
                    // `STARTUPINFO.wShowWindow` instead of `nCmdShow`.
                    // A hidden startup state can hide the settings window.
                    // `SetWindowPos` sets `WS_VISIBLE` directly and avoids that state.
                    SWP_NOMOVE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
            }
        }
    }

    unsafe fn build_entry(
        &mut self,
        spec: &EntrySpec,
        form: &SettingsForm,
        stale: &[String],
        plugin_names: &mut Vec<String>,
        plugin_dirs: &mut Vec<PathBuf>,
        top: i32,
    ) -> Result<BuiltEntry> {
        // SAFETY: Every FFI call uses the live settings window and content pane.
        unsafe {
            let h = self.hwnd;
            let page = self.content;
            let f = self.font.get();
            let mut controls = Vec::new();
            let mut y = top;
            let mut help_rendered = false;

        macro_rules! labelled_row {
            ($class:expr, $text:expr, $style:expr, $id:expr, $height:expr) => {{
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(
                    page,
                    w!("STATIC"),
                    &spec.label,
                    WINDOW_STYLE(0),
                    PAD,
                    y + 4,
                    LABEL_W,
                    label_h,
                    0,
                    f,
                )?);
                let control = child(
                    page,
                    $class,
                    $text,
                    $style,
                    FIELD_X,
                    y,
                    FIELD_W,
                    $height,
                    $id,
                    f,
                )?;
                controls.push(control);
                y += label_h.max(ROW_H) + ROW_GAP;
                control
            }};
        }

        macro_rules! checkbox {
            ($id:expr, $checked:expr) => {{
                let control = child(
                    page,
                    w!("BUTTON"),
                    &spec.label,
                    WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                    PAD,
                    y,
                    WIN_W - 2 * PAD - 20,
                    ROW_H,
                    $id,
                    f,
                )?;
                SendMessageW(
                    control,
                    BM_SETCHECK,
                    Some(WPARAM(if $checked { 1 } else { 0 })),
                    None,
                );
                controls.push(control);
                y += ROW_H + ROW_GAP;
                control
            }};
        }

        macro_rules! help {
            () => {{
                help_rendered = true;
                if let Some(text) = spec.help.as_deref() {
                    let height = measured_text_height(h, f, text, WIN_W - 2 * PAD - 20);
                    controls.push(child(
                        page,
                        w!("STATIC"),
                        text,
                        WINDOW_STYLE(0),
                        PAD,
                        y,
                        WIN_W - 2 * PAD - 20,
                        height,
                        0,
                        f,
                    )?);
                    y += height + ROW_GAP;
                }
            }};
        }

        match spec.id {
            SettingId::ClosePopup => {
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                controls.push(child(page, w!("STATIC"), "Escape", WINDOW_STYLE(0), FIELD_X,
                    y + 4, FIELD_W, ROW_H, 0, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
                help!();
            }
            SettingId::LookupMode => {
                let label_h = measured_text_height(h, f, &spec.label, WIN_W - 2 * PAD - 20);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, WIN_W - 2 * PAD - 20, label_h, 0, f)?);
                y += label_h;
                let is_live = matches!(form.cfg.trigger.mode, crate::config::TriggerMode::Live);
                let is_toggle = matches!(form.cfg.trigger.mode, crate::config::TriggerMode::Toggle);
                let is_press = matches!(form.cfg.trigger.mode, crate::config::TriggerMode::Press);
                let is_hold = !is_live && !is_toggle && !is_press;
                for (index, (text, id, checked)) in [
                    ("Live", ID_MODE_LIVE, is_live),
                    ("Hold key", ID_MODE_HOLD, is_hold),
                    ("Toggle", ID_MODE_TOGGLE, is_toggle),
                    ("Press key", ID_MODE_PRESS, is_press),
                ]
                .into_iter()
                .enumerate()
                {
                    let style = WINDOW_STYLE(BS_AUTORADIOBUTTON as u32)
                        | if index == 0 { WS_GROUP | WS_TABSTOP } else { WINDOW_STYLE(0) };
                    let control = child(page, w!("BUTTON"), text, style,
                        PAD + index as i32 * 130, y, 120, ROW_H, id, f)?;
                    SendMessageW(control, BM_SETCHECK,
                        Some(WPARAM(if checked { 1 } else { 0 })), None);
                    controls.push(control);
                }
                y += ROW_H + ROW_GAP;
            }
            SettingId::LookupKey => {
                let key_vk = crate::config::parse_trigger_key(&form.cfg.trigger.trigger_key)
                    .unwrap_or(0x10);
                CAPTURED_VK.with(|cell| cell.set(Some((h.0 as isize, key_vk))));
                let key_name = crate::config::trigger_key_name(key_vk);
                let button = labelled_row!(w!("BUTTON"), &key_name, WS_TABSTOP,
                    ID_TRIGGER_KEY, ROW_H);
                let is_live = matches!(form.cfg.trigger.mode, crate::config::TriggerMode::Live);
                let _ = EnableWindow(button, !is_live);
            }
            SettingId::AnkiAddKey => {
                let parsed = crate::config::parse_trigger_key(&form.cfg.anki.add_key);
                ANKI_CAPTURED_VK.with(|cell| {
                    cell.set(parsed.map(|vk| (h.0 as isize, vk)));
                });
                let name = parsed.map(crate::config::trigger_key_name)
                    .unwrap_or_else(|| "Not set".to_string());
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                controls.push(child(page, w!("BUTTON"), &name, WS_TABSTOP, FIELD_X, y,
                    FIELD_W - 80, ROW_H, ID_ANKI_ADD_KEY, f)?);
                controls.push(child(page, w!("BUTTON"), "Clear", WS_TABSTOP,
                    FIELD_X + FIELD_W - 72, y, 72, ROW_H, ID_ANKI_ADD_KEY_CLEAR, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
            }
            SettingId::StaticRegionKey => {
                let parsed = crate::config::parse_trigger_key(&form.cfg.anki.static_region_key);
                SR_CAPTURED_VK.with(|cell| {
                    cell.set(parsed.map(|vk| (h.0 as isize, vk)));
                });
                let name = parsed.map(crate::config::trigger_key_name)
                    .unwrap_or_else(|| "Not set".to_string());
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, ID_STATIC_REGION_LABEL, f)?);
                controls.push(child(page, w!("BUTTON"), &name, WS_TABSTOP, FIELD_X, y,
                    FIELD_W - 80, ROW_H, ID_STATIC_REGION_KEY, f)?);
                controls.push(child(page, w!("BUTTON"), "Clear", WS_TABSTOP,
                    FIELD_X + FIELD_W - 72, y, 72, ROW_H, ID_STATIC_REGION_KEY_CLEAR, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
            }
            SettingId::ScreenshotKey => {
                SCREENSHOT_CAPTURED_VK.with(|cell| cell.set(None));
                let name = crate::config::parse_trigger_key(&form.cfg.actions.screenshot.hotkey)
                    .map(crate::config::trigger_key_name)
                    .unwrap_or_else(|| {
                        if form.cfg.actions.screenshot.hotkey.is_empty() {
                            "Not set".to_string()
                        } else {
                            form.cfg.actions.screenshot.hotkey.clone()
                        }
                    });
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                controls.push(child(page, w!("BUTTON"), &name, WS_TABSTOP, FIELD_X, y,
                    FIELD_W - 80, ROW_H, ID_SCREENSHOT_HOTKEY, f)?);
                controls.push(child(page, w!("BUTTON"), "Clear", WS_TABSTOP,
                    FIELD_X + FIELD_W - 72, y, 72, ROW_H, ID_SCREENSHOT_KEY_CLEAR, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
                help!();
            }
            SettingId::OcrClipboardKey => {
                let parsed = crate::config::parse_trigger_key(
                    form.ocr_clipboard_key.as_deref().unwrap_or(""),
                );
                OCR_CLIP_CAPTURED_VK.with(|cell| {
                    cell.set(parsed.map(|vk| (h.0 as isize, vk)));
                });
                let name = parsed.map(crate::config::trigger_key_name)
                    .unwrap_or_else(|| "Not set".to_string());
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                controls.push(child(page, w!("BUTTON"), &name, WS_TABSTOP, FIELD_X, y,
                    FIELD_W - 80, ROW_H, ID_OCR_CLIPBOARD_KEY, f)?);
                controls.push(child(page, w!("BUTTON"), "Clear", WS_TABSTOP,
                    FIELD_X + FIELD_W - 72, y, 72, ROW_H, ID_OCR_CLIPBOARD_KEY_CLEAR, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
            }
            SettingId::SearchKey | SettingId::SentenceSearchKey => {
                let dictionary = spec.id == SettingId::SearchKey;
                let (id, clear, value) = if dictionary {
                    (ID_SEARCH_KEY, ID_SEARCH_KEY_CLEAR, form.cfg.actions.search.hotkey.as_deref())
                } else { (ID_SENTENCE_SEARCH_KEY, ID_SENTENCE_SEARCH_KEY_CLEAR,
                    form.cfg.actions.search.sentence_hotkey.as_deref()) };
                let name = value.filter(|key| !key.is_empty()).unwrap_or("Not set");
                let cell = if dictionary { &SEARCH_CAPTURED } else { &SENTENCE_SEARCH_CAPTURED };
                cell.with(|cell| *cell.borrow_mut() = None);
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                controls.push(child(page, w!("BUTTON"), name, WS_TABSTOP, FIELD_X, y,
                    FIELD_W - 80, ROW_H, id, f)?);
                controls.push(child(page, w!("BUTTON"), "Clear", WS_TABSTOP,
                    FIELD_X + FIELD_W - 72, y, 72, ROW_H, clear, f)?);
                y += label_h.max(ROW_H) + ROW_GAP;
                help!();
            }
            SettingId::OpenDictionarySearch | SettingId::OpenSentenceSearch => {
                let id = if spec.id == SettingId::OpenDictionarySearch { ID_OPEN_DICTIONARY_SEARCH }
                    else { ID_OPEN_SENTENCE_SEARCH };
                controls.push(child(page, w!("BUTTON"), &spec.label, WS_TABSTOP, PAD, y,
                    FIELD_W, ROW_H, id, f)?);
                y += ROW_H + ROW_GAP;
            }
            SettingId::OcrSentenceSearch => {
                checkbox!(ID_OCR_SENTENCE_SEARCH, form.cfg.actions.ocr_clipboard.as_ref()
                    .is_some_and(|action| action.open_sentence_search));
            }
            SettingId::PopupSubPopups => { checkbox!(ID_SUB_POPUPS, form.cfg.popup.sub_popups); }
            SettingId::PopupTheme => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_THEME, 220);
                for (index, name) in ["dark", "light"].into_iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(name).as_ptr() as isize)));
                    if form.cfg.popup.theme == name {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
                if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
                }
            }
            SettingId::PopupFont => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_FONT, 260);
                let mut families = japanese_font_families();
                if !families.iter().any(|name| name == &form.cfg.popup.font) {
                    families.push(form.cfg.popup.font.clone());
                    families.sort();
                }
                for (index, name) in families.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(name).as_ptr() as isize)));
                    if name == &form.cfg.popup.font {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
                self.fonts = families;
            }
            SettingId::PopupCustomStyle => {
                controls.push(child(page, w!("BUTTON"), &spec.label, WS_TABSTOP, PAD, y,
                    WIN_W - 2 * PAD - 20, ROW_H, ID_CSS_EDITOR, f)?);
                y += ROW_H + ROW_GAP;
            }
            SettingId::PopupMaxWidth => {
                self.widths = numeric_choices(MAX_WIDTH_RANGE.0 as i64,
                    MAX_WIDTH_RANGE.1 as i64, 5, form.cfg.popup.max_width_percent as i64);
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_MAX_WIDTH, 220);
                fill_numeric(combo, &self.widths, form.cfg.popup.max_width_percent as i64);
                help!();
            }
            SettingId::PopupMaxHeight => {
                self.heights = numeric_choices(MAX_HEIGHT_RANGE.0 as i64,
                    MAX_HEIGHT_RANGE.1 as i64, 5, form.cfg.popup.max_height_percent as i64);
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_MAX_HEIGHT, 220);
                fill_numeric(combo, &self.heights, form.cfg.popup.max_height_percent as i64);
                help!();
            }
            SettingId::PopupSummaryLength => {
                self.summaries = numeric_choices(SUMMARY_RANGE.0 as i64,
                    SUMMARY_RANGE.1 as i64, 10, form.cfg.popup.summary_chars as i64);
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_SUMMARY, 220);
                fill_numeric(combo, &self.summaries, form.cfg.popup.summary_chars as i64);
                help!();
            }
            SettingId::PopupHighlight => { checkbox!(ID_HIGHLIGHT, form.cfg.popup.highlight_match); }
            SettingId::PopupScroll => { checkbox!(ID_SCROLL, form.cfg.popup.scroll_popup); }
            SettingId::PopupEdgeAutoscroll => {
                checkbox!(ID_EDGE_AUTOSCROLL, form.cfg.popup.edge_autoscroll);
            }
            SettingId::PopupSidePanel => { checkbox!(ID_SIDE_PANEL, form.cfg.popup.side_panel); }
            SettingId::PopupCaptureExclusion => {
                checkbox!(ID_EXCLUDE, form.cfg.popup.exclude_from_capture);
            }
            SettingId::PopupLayout => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_LAYOUT_MODE, 220);
                for (index, (mode, text)) in LAYOUT_MODES.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(text).as_ptr() as isize)));
                    if form.cfg.popup.layout_mode == *mode {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
                if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
                }
            }
            SettingId::PopupDictionaryStyling => {
                checkbox!(ID_DICT_STYLING, form.cfg.popup.dictionary_styling);
            }
            SettingId::PopupExamples => { checkbox!(ID_SHOW_EXAMPLES, form.cfg.popup.show_examples); }
            SettingId::PopupAttributions => {
                checkbox!(ID_SHOW_ATTRIBUTIONS, form.cfg.popup.show_attributions);
            }
            SettingId::PopupImages => { checkbox!(ID_SHOW_IMAGES, form.cfg.popup.show_images); }
            SettingId::PopupPartOfSpeech => {
                checkbox!(ID_SHOW_POS, form.cfg.popup.show_part_of_speech);
            }
            SettingId::DictionaryTerms
            | SettingId::DictionaryFrequency
            | SettingId::DictionaryPitch => {
                let role = match spec.id {
                    SettingId::DictionaryTerms => Role::Terms,
                    SettingId::DictionaryFrequency => Role::Frequency,
                    SettingId::DictionaryPitch => Role::Pitch,
                    _ => unreachable!(),
                };
                let section = SECTIONS.iter().find(|section| section.role == role)
                    .expect("dictionary role should exist");
                let title_h = measured_text_height(h, f, &spec.label, WIN_W - 2 * PAD - 20);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y, WIN_W - 2 * PAD - 20, title_h, 0, f)?);
                y += title_h;
                help!();
                let button_x = WIN_W - PAD - BTN_W - 8;
                let list_w = button_x - 2 * PAD + 4;
                if role == Role::Frequency {
                    controls.push(child(page, w!("STATIC"), "Combine ranks by",
                        WINDOW_STYLE(0), PAD, y + 4, 110, ROW_H, 0, f)?);
                    let ranking = child(page, w!("COMBOBOX"), "",
                        WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                        PAD + 114, y, list_w - 114, 120, ID_RANKING, f)?;
                    controls.push(ranking);
                    for (index, (strategy, text)) in RANKING_STRATEGIES.iter().enumerate() {
                        SendMessageW(ranking, CB_ADDSTRING, None,
                            Some(LPARAM(wide(text).as_ptr() as isize)));
                        if *strategy == form.cfg.dictionaries.ranking_strategy {
                            SendMessageW(ranking, CB_SETCURSEL, Some(WPARAM(index)), None);
                        }
                    }
                    if SendMessageW(ranking, CB_GETCURSEL, None, None).0 < 0 {
                        SendMessageW(ranking, CB_SETCURSEL, Some(WPARAM(0)), None);
                    }
                    y += ROW_H + ROW_GAP;
                }
                let list = make_role_list(page, y, list_w, section.list, f)?;
                controls.push(list);
                fill_role_list(list, form.list(role), 0);
                for (row, (text, id)) in [
                    ("Move up", section.up),
                    ("Move down", section.down),
                    ("Add\u{2026}", section.add),
                    ("Remove", section.remove),
                ]
                .into_iter()
                .enumerate()
                {
                    controls.push(child(page, w!("BUTTON"), text, WS_TABSTOP, button_x,
                        y + row as i32 * BTN_PITCH, BTN_W, ROW_H, id, f)?);
                }
                y += DICT_LIST_H + 8;
                if spec.id == SettingId::DictionaryTerms
                    && form.library_empty
                    && !form.terms.is_empty()
                {
                    let text = "chibipop is using a dictionary built outside the app. Adding or \
                        removing here rebuilds from this list only. Import the original ZIP files first.";
                    let height = measured_text_height(h, f, text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, 0, f)?);
                    y += height + ROW_GAP;
                }
                if spec.id == SettingId::DictionaryTerms && !stale.is_empty() {
                    let text = format!(
                        "\"{}\" names no installed dictionary. Its place is kept; remove the row if it is gone.",
                        stale.join("\", \"")
                    );
                    let height = measured_text_height(h, f, &text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), &text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, 0, f)?);
                    y += height + ROW_GAP;
                }
            }
            SettingId::OcrEngine => {
                let found = crate::plugin::discover::discover(&crate::paths::beside_exe("plugins"));
                let mut names = vec!["builtin".to_string()];
                names.extend(discovered_text_providers(&found));
                if form.cfg.ocr.engine != "builtin" && !names.contains(&form.cfg.ocr.engine) {
                    names.push(form.cfg.ocr.engine.clone());
                }
                let label_h = measured_text_height(h, f, &spec.label, LABEL_W);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y + 4, LABEL_W, label_h, 0, f)?);
                let combo = child(page, w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    FIELD_X, y, FIELD_W - BTN_W - 8, 220, ID_ENGINE, f)?;
                controls.push(combo);
                for name in &names {
                    let shown = if name == "builtin" { "Built-in (Windows OCR)" } else { name };
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(shown).as_ptr() as isize)));
                }
                let index = names.iter().position(|name| name == &form.cfg.ocr.engine)
                    .unwrap_or(0);
                SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                self.engine_names = names;
                self.engine_dirs = first_provider_directories(found.iter().filter_map(
                    |(dir, parsed)| {
                        let manifest = parsed.as_ref().ok()?;
                        manifest.roles
                            .contains(&crate::plugin::manifest::Role::TextProvider)
                            .then(|| (manifest.name.clone(), dir.clone()))
                    },
                ));
                let configure = child(page, w!("BUTTON"), "Configure\u{2026}", WS_TABSTOP,
                    FIELD_X + FIELD_W - BTN_W, y, BTN_W, ROW_H, ID_ENGINE_CONFIGURE, f)?;
                controls.push(configure);
                let _ = ShowWindow(configure, SW_HIDE);
                y += label_h.max(ROW_H) + ROW_GAP;
            }
            SettingId::OcrLanguage => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_OCR_LANG, 220);
                let languages = language_choices(
                    crate::text::ocr::installed_recognisers(),
                    &form.cfg.ocr.language,
                );
                for (name, _) in &languages {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(name).as_ptr() as isize)));
                }
                if let Some(index) = language_index(&languages, &form.cfg.ocr.language) {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                }
                self.ocr_langs = languages.into_iter().map(|(_, tag)| tag).collect();
                let engine_index = dlg_item(h, ID_ENGINE)
                    .map(|engine| SendMessageW(engine, CB_GETCURSEL, None, None).0)
                    .unwrap_or(0);
                let _ = EnableWindow(combo, engine_index <= 0);
                help!();
            }
            SettingId::OcrPasses => {
                self.passes = numeric_choices(PASSES_RANGE.0 as i64, PASSES_RANGE.1 as i64,
                    1, form.cfg.ocr.max_ocr_passes as i64);
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_PASSES, 160);
                fill_numeric(combo, &self.passes, form.cfg.ocr.max_ocr_passes as i64);
                help!();
            }
            SettingId::OcrCaptureSize => {
                let title_h = measured_text_height(h, f, &spec.label, WIN_W - 2 * PAD - 20);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD,
                    y, WIN_W - 2 * PAD - 20, title_h, 0, f)?);
                y += title_h;
                for (label, id, value) in [
                    ("Width (px)", ID_CAPTURE_W, form.cfg.ocr.capture_width),
                    ("Height (px)", ID_CAPTURE_H, form.cfg.ocr.capture_height),
                ] {
                    controls.push(child(page, w!("STATIC"), label, WINDOW_STYLE(0), PAD,
                        y + 4, LABEL_W, ROW_H, 0, f)?);
                    controls.push(child(page, w!("EDIT"), &value.to_string(),
                        WS_TABSTOP | WS_BORDER, FIELD_X, y, FIELD_W, ROW_H, id, f)?);
                    y += ROW_H + ROW_GAP;
                }
                help!();
            }
            SettingId::OcrPreferVertical => {
                checkbox!(ID_PREFER_VERT, form.cfg.ocr.prefer_vertical);
            }
            SettingId::OcrScanAlphanumeric => {
                checkbox!(ID_SCAN_ALNUM, form.cfg.ocr.scan_alphanumeric);
            }
            SettingId::OcrDiscardFurigana => {
                checkbox!(ID_DISCARD_FURIGANA, form.cfg.ocr.discard_furigana);
            }
            SettingId::OcrPerCharacter => {
                let control = checkbox!(ID_PER_CHAR, form.cfg.trigger.per_character_lookup);
                let is_live = matches!(form.cfg.trigger.mode, crate::config::TriggerMode::Live);
                let _ = EnableWindow(control, is_live);
                help!();
            }
            SettingId::DebugCaptureOutline => {
                checkbox!(ID_SHOW_SCAN, form.cfg.debug.show_scan_region);
            }
            SettingId::DebugEngine => {
                checkbox!(ID_ENGINE_LOG, form.cfg.debug.show_engine_log);
            }
            SettingId::DebugAdapter => {
                checkbox!(ID_ADAPTER_LOG, form.cfg.debug.show_adapter_log);
            }
            SettingId::ShowLiveLogs => {
                controls.push(child(page, w!("BUTTON"), &spec.label, WS_TABSTOP, PAD, y,
                    160, ROW_H, ID_SHOW_LIVE_LOGS, f)?);
                y += ROW_H + ROW_GAP;
            }
            SettingId::AnkiEnabled => { checkbox!(ID_ANKI_ENABLED, form.cfg.anki.enabled); }
            SettingId::AnkiNotifyOnAdd => {
                checkbox!(ID_NOTIFY_ON_ADD, form.cfg.anki.notify_on_add);
            }
            SettingId::AnkiUrl => {
                labelled_row!(w!("EDIT"), &form.cfg.anki.url, WS_TABSTOP | WS_BORDER,
                    ID_ANKI_URL, ROW_H);
            }
            SettingId::AnkiDeck => {
                let combo = labelled_row!(w!("COMBOBOX"), &form.cfg.anki.deck,
                    WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_ANKI_DECK, 160);
                SendMessageW(combo, WM_SETTEXT, None,
                    Some(LPARAM(wide(&form.cfg.anki.deck).as_ptr() as isize)));
            }
            SettingId::AnkiModel => {
                let combo = labelled_row!(w!("COMBOBOX"), &form.cfg.anki.model,
                    WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_ANKI_MODEL, 160);
                SendMessageW(combo, WM_SETTEXT, None,
                    Some(LPARAM(wide(&form.cfg.anki.model).as_ptr() as isize)));
            }
            SettingId::AnkiRefresh => {
                controls.push(child(page, w!("BUTTON"), &spec.label, WS_TABSTOP, PAD, y,
                    160, ROW_H, ID_ANKI_TEST, f)?);
                if let Some(text) = spec.help.as_deref() {
                    help_rendered = true;
                    let width = WIN_W - PAD - (PAD + 168) - 20;
                    let height = measured_text_height(h, f, text, width);
                    controls.push(child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD + 168,
                        y + 2, width, height, 0, f)?);
                    y += height.max(ROW_H) + ROW_GAP;
                } else {
                    help_rendered = true;
                    y += ROW_H + ROW_GAP;
                }
            }
            SettingId::AnkiIncludeScreenshot => {
                checkbox!(ID_INCLUDE_SCREENSHOT, form.cfg.actions.screenshot.include_on_add);
            }
            SettingId::ScreenshotTargets => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_SCREENSHOT_MODE, 180);
                for (index, mode) in ScreenshotMode::ALL.iter().enumerate() {
                    let label = mode.to_string();
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(&label).as_ptr() as isize)));
                    if *mode == form.cfg.actions.screenshot.capture_mode {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
                if SendMessageW(combo, CB_GETCURSEL, None, None).0 < 0 {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(0)), None);
                }
                help_rendered = true;
                if let Some(text) = spec.help.as_deref() {
                    let height = measured_text_height(h, f, text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, ID_SCREENSHOT_HINT, f)?);
                    y += height + ROW_GAP;
                }
                let summary = screenshot_target_summary(form);
                let summary_h = measured_text_height(h, f, &summary,
                    WIN_W - 2 * PAD - BTN_W - 28);
                self.screenshot_summary_height.set(summary_h);
                controls.push(child(page, w!("STATIC"), &summary, WINDOW_STYLE(0), PAD, y,
                    WIN_W - 2 * PAD - BTN_W - 28, summary_h, ID_SCREENSHOT_SUMMARY, f)?);
                let reset = child(page, w!("BUTTON"), "Clear saved targets", WS_TABSTOP,
                    WIN_W - PAD - BTN_W - 8, y, BTN_W, ROW_H, ID_SCREENSHOT_RESET, f)?;
                let has_target = form.cfg.actions.screenshot.fixed_region.is_some()
                    || form.cfg.actions.screenshot.fixed_window.is_some();
                let _ = EnableWindow(reset, has_target);
                controls.push(reset);
                y += summary_h.max(ROW_H) + ROW_GAP;
            }
            SettingId::AnkiIncludeDictionaryName => {
                checkbox!(ID_INCLUDE_DICTIONARY_NAME, form.cfg.anki.include_dictionary_name);
            }
            SettingId::AnkiFirstDictionaryOnly => {
                checkbox!(ID_FIRST_DICT_ONLY, form.cfg.anki.first_dict_only);
            }
            SettingId::AnkiSelectionButtons => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_SELECTION_BUTTONS, 160);
                for (index, (value, text)) in SELECTION_BUTTONS.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(text).as_ptr() as isize)));
                    if *value == form.cfg.anki.selection_buttons {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
            }
            SettingId::AnkiSelectionSeparator => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_SELECTION_SEPARATOR, 180);
                for (index, (value, text)) in SELECTION_SEPARATORS.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(text).as_ptr() as isize)));
                    if *value == form.cfg.anki.selection_separator {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
            }
            SettingId::AnkiTripleClick => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_TRIPLE_CLICK, 180);
                for (index, (value, text)) in TRIPLE_CLICKS.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(text).as_ptr() as isize)));
                    if *value == form.cfg.anki.triple_click {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
            }
            SettingId::AnkiSentenceMode => {
                let combo = labelled_row!(w!("COMBOBOX"), "",
                    WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                    ID_SENTENCE_MODE, 180);
                for (index, (value, text)) in SENTENCE_MODES.iter().enumerate() {
                    SendMessageW(combo, CB_ADDSTRING, None,
                        Some(LPARAM(wide(text).as_ptr() as isize)));
                    if *value == form.cfg.anki.sentence_mode {
                        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
                    }
                }
            }
            SettingId::AnkiStaticOverlay => {
                checkbox!(ID_SHOW_STATIC_OVERLAY, form.cfg.anki.show_static_overlay);
                help_rendered = true;
                if let Some(text) = spec.help.as_deref() {
                    let height = measured_text_height(h, f, text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, ID_STATIC_CAPTURE_HINT, f)?);
                    y += height + ROW_GAP;
                }
            }
            SettingId::AnkiFieldMap => {
                let text = field_map_toggle_label(&spec.label, self.field_map_collapsed.get());
                controls.push(child(page, w!("BUTTON"), &text, WS_TABSTOP, PAD, y,
                    200, ROW_H, ID_FIELD_MAP_TOGGLE, f)?);
                y += ROW_H + ROW_GAP;
                help!();
            }
            SettingId::PluginList => {
                let root = crate::paths::beside_exe("plugins");
                let found = crate::plugin::discover::discover(&root);
                let title_h = measured_text_height(h, f, &spec.label, WIN_W - 2 * PAD - 20);
                controls.push(child(page, w!("STATIC"), &spec.label, WINDOW_STYLE(0), PAD, y,
                    WIN_W - 2 * PAD - 20, title_h, 0, f)?);
                y += title_h + ROW_GAP;
                let enabled = form.cfg.plugins.enabled.clone();
                let button_x = WIN_W - PAD - BTN_W - 8;
                if found.is_empty() {
                    let text = format!("No plugins found in {}.", root.display());
                    let height = measured_text_height(h, f, &text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), &text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, 0, f)?);
                    y += height + ROW_GAP;
                } else {
                    for (index, (dir, parsed)) in found.iter().enumerate() {
                        if index > 0 {
                            y += ROW_GAP;
                        }
                        let row = plugin_row(dir, parsed, &enabled);
                        plugin_names.push(plugin_key(dir, parsed));
                        plugin_dirs.push(dir.clone());
                        controls.push(child(page, w!("STATIC"), &row.label, WINDOW_STYLE(0), PAD,
                            y + 4, button_x - PAD - 8, ROW_H, 0, f)?);
                        let checkbox = child(page, w!("BUTTON"), "Enable",
                            WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                            button_x, y, BTN_W, ROW_H,
                            ID_PLUGIN_ENABLE_BASE + index as i32, f)?;
                        SendMessageW(checkbox, BM_SETCHECK,
                            Some(WPARAM(if row.checked { 1 } else { 0 })), None);
                        let _ = EnableWindow(checkbox, row.can_enable);
                        controls.push(checkbox);
                        controls.push(child(page, w!("STATIC"), &row.roles, WINDOW_STYLE(0), PAD,
                            y + ROW_H + 4, button_x - PAD - 8, ROW_H, 0, f)?);
                        let status_y = y + 2 * ROW_H;
                        controls.push(child(page, w!("STATIC"), &row.status, WINDOW_STYLE(0), PAD,
                            status_y, button_x - PAD - 8, PLUGIN_STATUS_H, 0, f)?);
                        controls.push(child(page, w!("BUTTON"), "Configure", WS_TABSTOP,
                            button_x, status_y, BTN_W, ROW_H,
                            ID_PLUGIN_CONFIGURE_BASE + index as i32, f)?);
                        y += PLUGIN_ROW_H;
                    }
                }
            }
        }

            if !help_rendered {
                if let Some(text) = spec.help.as_deref() {
                    let height = measured_text_height(h, f, text, WIN_W - 2 * PAD - 20);
                    controls.push(child(page, w!("STATIC"), text, WINDOW_STYLE(0), PAD, y,
                        WIN_W - 2 * PAD - 20, height, 0, f)?);
                    y += height + ROW_GAP;
                }
            }

            if let Some(&control) = controls.first() {
                let style = GetWindowLongW(control, GWL_STYLE) as u32 | WS_GROUP.0;
                SetWindowLongW(control, GWL_STYLE, style as i32);
            }

            Ok(BuiltEntry {
                id: spec.id,
                label: spec.label.clone(),
                controls,
                top,
                height: y - top,
            })
        }
    }

    /// Builds settings controls.
    unsafe fn build(
        &mut self,
        form: &SettingsForm,
        stale: &[String],
        layout: &SettingsLayout,
    ) -> Result<i32> {
        let f = self.font.get();
        let h = self.hwnd;
        // SAFETY: The main window owns every created child.
        unsafe {
            let controls = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES,
            };
            let _ = InitCommonControlsEx(&controls);
            let tab_control = child(
                h,
                w!("SysTabControl32"),
                "",
                WS_TABSTOP | WS_CLIPSIBLINGS,
                PAD - 6,
                PAD,
                WIN_W - 2 * PAD,
                TAB_H,
                ID_TAB,
                f,
            )?;
            for (index, tab) in layout.tabs.iter().enumerate() {
                let mut text = wide(&tab.label);
                let item = TcItemW {
                    mask: TCIF_TEXT_VAL,
                    dw_state: 0,
                    dw_state_mask: 0,
                    psz_text: text.as_mut_ptr(),
                    cch_text_max: 0,
                    i_image: -1,
                    l_param: 0,
                };
                SendMessageW(
                    tab_control,
                    TCM_INSERTITEMW_MSG,
                    Some(WPARAM(index)),
                    Some(LPARAM(&item as *const _ as isize)),
                );
            }

            self.viewport = child(
                h,
                pane_class_name(),
                "",
                WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                0,
                CONTENT_Y,
                WIN_W,
                0,
                ID_VIEWPORT,
                None,
            )?;
            self.content = child(
                self.viewport,
                pane_class_name(),
                "",
                WS_CLIPSIBLINGS,
                0,
                0,
                WIN_W,
                0,
                ID_CONTENT,
                None,
            )?;
            for pane in [self.viewport, self.content] {
                let ex = GetWindowLongW(pane, GWL_EXSTYLE) as u32 | WS_EX_CONTROLPARENT.0;
                SetWindowLongW(pane, GWL_EXSTYLE, ex as i32);
            }
        }

        let mut runtime_tabs = Vec::with_capacity(layout.tabs.len());
        let mut plugin_names = Vec::new();
        let mut plugin_dirs = Vec::new();
        for tab in &layout.tabs {
            let mut y = 0;
            let mut runtime_sections = Vec::with_capacity(tab.sections.len());
            for section in &tab.sections {
                let section_top = y;
                // SAFETY: The content pane owns this frame.
                let frame = unsafe {
                    child(
                        self.content,
                        w!("BUTTON"),
                        &section.label,
                        WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
                        PAD - 6,
                        section_top,
                        WIN_W - 2 * PAD,
                        28,
                        0,
                        f,
                    )?
                };
                y += 20;
                let mut runtime_entries = Vec::with_capacity(section.entries.len());
                for entry in &section.entries {
                    // SAFETY: Each builder creates children of the live content pane.
                    let built = unsafe {
                        self.build_entry(
                            entry,
                            form,
                            stale,
                            &mut plugin_names,
                            &mut plugin_dirs,
                            y,
                        )?
                    };
                    y += built.height;
                    let controls = built.controls.into_iter().map(|control| {
                        // SAFETY: `build_entry` created each live child.
                        unsafe {
                            capture_control_runtime(self.content, built.top, control, self.font.get())
                        }
                    }).collect();
                    runtime_entries.push(EntryRuntime {
                        id: built.id,
                        label: built.label,
                        controls,
                        top: Cell::new(built.top),
                        base_height: Cell::new(built.height),
                        initial_height: built.height,
                    });
                }
                let section_height = (y - section_top + 8).max(28);
                // SAFETY: The frame remains a live child.
                unsafe {
                    let _ = SetWindowPos(
                        frame,
                        None,
                        0,
                        0,
                        dpi_scale(h, WIN_W - 2 * PAD),
                        dpi_scale(h, section_height),
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                runtime_sections.push(SectionRuntime {
                    frame,
                    top: Cell::new(section_top),
                    height: Cell::new(section_height),
                    entries: runtime_entries,
                });
                y += GROUP_GAP;
            }
            runtime_tabs.push(TabRuntime {
                id: tab.id,
                label: tab.label.clone(),
                sections: runtime_sections,
                page_height: Cell::new(y),
            });
        }
        self.tabs = runtime_tabs;
        debug_assert_eq!(layout.tabs.len(), self.tabs.len());
        for index in 0..layout.tabs.len() {
            debug_assert_eq!(Some(layout.tabs[index].id), self.tab_id(index as u32));
            debug_assert_eq!(Some(layout.tabs[index].label.as_str()), self.tab_label(index as u32));
        }
        self.plugin_names = plugin_names;
        remember_plugin_dirs(h, plugin_dirs);

        remember_conditional_tabs(
            h,
            ConditionalTabs {
                engine: self.entry_tab(SettingId::OcrEngine),
                static_key: self.entry_tab(SettingId::StaticRegionKey),
                static_overlay: self.entry_tab(SettingId::AnkiStaticOverlay),
            },
        );
        self.reflow_all_tabs();
        self.bottom_y0 = self.layout_bottom();

        // SAFETY: Bottom controls are direct children of the live main window.
        unsafe {
            child(
                h,
                w!("BUTTON"),
                "Status",
                WINDOW_STYLE(BS_GROUPBOX as u32),
                PAD - 6,
                self.bottom_y0,
                WIN_W - 2 * PAD,
                BOTTOM_H - 8,
                ID_UPDATES,
                f,
            )?;
            child(
                h,
                w!("BUTTON"),
                "Check for updates",
                WS_TABSTOP,
                PAD,
                self.bottom_y0 + BOTTOM_UPDATE_DY,
                136,
                ROW_H,
                ID_CHECK_UPDATE,
                f,
            )?;
            let apply_state = format!("Apply: {}", self.apply_state.get().label());
            child(
                h,
                w!("STATIC"),
                &apply_state,
                WINDOW_STYLE(0),
                PAD,
                self.bottom_y0 + BOTTOM_APPLY_STATE_DY,
                WIN_W - 2 * PAD - 16,
                ROW_H,
                ID_APPLY_STATE,
                f,
            )?;
            let runtime = match self.apply_mode {
                ApplyMode::Live => "OCR status is initializing.",
                ApplyMode::Standalone => "Scanning is inactive.",
            };
            child(
                h,
                w!("STATIC"),
                runtime,
                WINDOW_STYLE(0),
                PAD,
                self.bottom_y0 + BOTTOM_RUNTIME_DY,
                WIN_W - 2 * PAD - 16,
                ROW_H,
                ID_RUNTIME_STATUS,
                f,
            )?;
            child(
                h,
                w!("EDIT"),
                "Ready.",
                WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32) | WS_BORDER | WS_VSCROLL,
                PAD,
                self.bottom_y0 + BOTTOM_STATUS_DY,
                WIN_W - 2 * PAD - 16,
                STATUS_H,
                ID_STATUS,
                f,
            )?;
            child(
                h,
                w!("BUTTON"),
                apply_caption(self.apply_mode),
                WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
                WIN_W - PAD - 144,
                self.bottom_y0 + BOTTOM_BTN_DY,
                136,
                ROW_H + 4,
                ID_APPLY,
                f,
            )?;
            child(
                h,
                w!("BUTTON"),
                "Quit chibipop",
                WS_TABSTOP,
                PAD,
                self.bottom_y0 + BOTTOM_BTN_DY,
                116,
                ROW_H + 4,
                ID_QUIT,
                f,
            )?;

            let band_h = self.bottom_y0 - CONTENT_Y;
            let _ = SetWindowPos(
                self.viewport,
                None,
                0,
                0,
                dpi_scale(h, WIN_W),
                dpi_scale(h, band_h),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = SetWindowPos(
                self.content,
                None,
                0,
                0,
                dpi_scale(h, WIN_W),
                dpi_scale(h, band_h),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.place_viewport();
            update_list_buttons(h);
        }
        self.switch_tab(0);
        Ok(self.bottom_y0 + BOTTOM_H)
    }
    /// Returns the current values of the controls as a form.
    pub fn read(&self, template: &SettingsForm) -> SettingsForm {
        // SAFETY: every id below names a live descendant of `self.hwnd`.
        // `build` makes each one, and `Drop` destroys it with the window.
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
                if i < 0 {
                    fallback
                } else {
                    *values.get(i as usize).unwrap_or(&fallback)
                }
            };
            let text_of =
                |id: i32| -> String { dlg_item(h, id).map(|c| window_text(c)).unwrap_or_default() };
            let px = |id: i32, fallback: i32| -> i32 { parse_px(&text_of(id), fallback) };

            // An empty list is valid. It is not an absent control.
            //
            // Each role list stores row order and enabled flags. Read the
            // ListView whenever its control exists. Use `template` only when
            // the control is absent.
            let role_rows = |id: i32, fallback: &[DictRow]| -> Vec<DictRow> {
                lv_rows(h, id).unwrap_or_else(|| fallback.to_vec())
            };
            let terms = role_rows(ID_TERMS, &template.terms);
            let frequency = role_rows(ID_FREQS, &template.frequency);
            let pitch = role_rows(ID_PITCH, &template.pitch);
            let staged = self.staged.borrow();
            let screenshot_reset_targets = staged.screenshot_reset_targets;

            let theme = if combo_index(ID_THEME) == 1 {
                "light"
            } else {
                "dark"
            };
            let sentence_mode = sentence_mode_at(combo_index(ID_SENTENCE_MODE));
            let selection_buttons = selection_buttons_at(combo_index(ID_SELECTION_BUTTONS));
            let screenshot_capture_mode = screenshot_mode_at(combo_index(ID_SCREENSHOT_MODE));
            let selection_separator = selection_separator_at(combo_index(ID_SELECTION_SEPARATOR));
            let triple_click = triple_click_at(combo_index(ID_TRIPLE_CLICK));
            let font = {
                let i = combo_index(ID_FONT);
                if i < 0 {
                    template.cfg.popup.font.clone()
                } else {
                    self.fonts
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.cfg.popup.font.clone())
                }
            };
            let ocr_language = {
                let i = combo_index(ID_OCR_LANG);
                if i < 0 {
                    template.cfg.ocr.language.clone()
                } else {
                    self.ocr_langs
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.cfg.ocr.language.clone())
                }
            };

            let engine = {
                let i = combo_index(ID_ENGINE);
                if i < 0 {
                    template.cfg.ocr.engine.clone()
                } else {
                    self.engine_names
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.cfg.ocr.engine.clone())
                }
            };

            let trigger_key = resolved_trigger_key(h, &template.cfg.trigger.trigger_key);
            let screenshot_hotkey = resolved_screenshot_key(h, &template.cfg.actions.screenshot.hotkey);
            let screenshot_hotkey_edited = screenshot_hotkey != template.cfg.actions.screenshot.hotkey;
            let anki_add_key = resolved_anki_add_key(h, &template.cfg.anki.add_key);
            let ocr_clipboard_key =
                resolved_ocr_clipboard_key(h, template.ocr_clipboard_key.as_deref());

            // Each row contains one field and its selected source.
            // Merge the row values with saved mappings.
            // Keep mappings for fields absent from the current model.
            // `"(none)"` removes a mapping for a visible field.
            // When no rows exist, keep the saved map unchanged.
            let rows = self.field_map_rows.borrow();
            let readings: Vec<(&str, &str)> = rows
                .iter()
                .map(|(name, combo)| {
                    let i = SendMessageW(*combo, CB_GETCURSEL, None, None).0.max(0);
                    let src = FIELD_MAP_SOURCES
                        .get(i as usize)
                        .copied()
                        .unwrap_or("(none)");
                    (name.as_str(), src)
                })
                .collect();
            let saved = template.field_map.as_deref().unwrap_or_default();
            let field_map = Some(merged_field_map(saved, &readings));

            let mut form = template.clone();
            form.cfg.trigger.mode = if checked(ID_MODE_PRESS) {
                crate::config::TriggerMode::Press
            } else if checked(ID_MODE_TOGGLE) {
                crate::config::TriggerMode::Toggle
            } else if checked(ID_MODE_HOLD) {
                crate::config::TriggerMode::HoldKey
            } else {
                crate::config::TriggerMode::Live
            };
            form.cfg.trigger.trigger_key = trigger_key;
            form.cfg.popup.theme = theme.to_string();
            form.cfg.popup.font = font;
            form.cfg.popup.max_width_percent =
                pick(&self.widths, ID_MAX_WIDTH, template.cfg.popup.max_width_percent as i64) as u8;
            form.cfg.popup.max_height_percent =
                pick(&self.heights, ID_MAX_HEIGHT, template.cfg.popup.max_height_percent as i64)
                    as u8;
            form.cfg.popup.summary_chars =
                pick(&self.summaries, ID_SUMMARY, template.cfg.popup.summary_chars as i64) as usize;
            form.cfg.popup.highlight_match = checked(ID_HIGHLIGHT);
            form.cfg.popup.scroll_popup = checked(ID_SCROLL);
            form.cfg.popup.edge_autoscroll = checked(ID_EDGE_AUTOSCROLL);
            form.cfg.popup.side_panel = checked(ID_SIDE_PANEL);
            form.cfg.popup.layout_mode = layout_mode_at(combo_index(ID_LAYOUT_MODE));
            form.cfg.popup.dictionary_styling = checked(ID_DICT_STYLING);
            form.cfg.popup.show_examples = checked(ID_SHOW_EXAMPLES);
            form.cfg.popup.show_attributions = checked(ID_SHOW_ATTRIBUTIONS);
            form.cfg.popup.show_images = checked(ID_SHOW_IMAGES);
            form.cfg.popup.show_part_of_speech = checked(ID_SHOW_POS);
            form.cfg.popup.exclude_from_capture = checked(ID_EXCLUDE);
            form.terms = terms;
            form.frequency = frequency;
            form.pitch = pitch;
            form.cfg.dictionaries.ranking_strategy = ranking_strategy_at(combo_index(ID_RANKING));
            form.dict_list_language = staged.dict_list_language.clone();
            form.cfg.dictionaries.per_language = staged.cfg.dictionaries.per_language.clone();
            form.cfg.ocr.max_ocr_passes =
                pick(&self.passes, ID_PASSES, template.cfg.ocr.max_ocr_passes as i64) as u8;
            form.cfg.ocr.prefer_vertical = checked(ID_PREFER_VERT);
            form.cfg.ocr.capture_width = px(ID_CAPTURE_W, template.cfg.ocr.capture_width);
            form.cfg.ocr.capture_height = px(ID_CAPTURE_H, template.cfg.ocr.capture_height);
            form.cfg.ocr.scan_alphanumeric = checked(ID_SCAN_ALNUM);
            form.cfg.ocr.discard_furigana = checked(ID_DISCARD_FURIGANA);
            form.cfg.trigger.per_character_lookup = checked(ID_PER_CHAR);
            form.cfg.ocr.language = ocr_language;
            form.cfg.ocr.engine = engine;
            form.cfg.debug.show_scan_region = checked(ID_SHOW_SCAN);
            form.cfg.debug.show_engine_log = checked(ID_ENGINE_LOG);
            form.cfg.debug.show_adapter_log = checked(ID_ADAPTER_LOG);
            form.freq_changed = staged.freq_changed;
            form.staged_adds = staged.staged_adds.clone();
            form.staged_removes = staged.staged_removes.clone();
            form.library_empty = staged.library_empty;
            form.unreadable = staged.unreadable.clone();
            form.cfg.anki.enabled = checked(ID_ANKI_ENABLED);
            form.cfg.anki.url = text_of(ID_ANKI_URL);
            form.cfg.anki.deck = text_of(ID_ANKI_DECK);
            form.cfg.anki.model = text_of(ID_ANKI_MODEL);
            form.cfg.anki.add_key = anki_add_key;
            form.field_map = field_map;
            form.cfg.anki.notify_on_add = checked(ID_NOTIFY_ON_ADD);
            form.cfg.anki.sentence_mode = sentence_mode;
            form.cfg.anki.static_region_key =
                resolved_sr_key(h, &template.cfg.anki.static_region_key);
            form.cfg.actions.screenshot.include_on_add = checked(ID_INCLUDE_SCREENSHOT);
            form.cfg.actions.screenshot.hotkey = screenshot_hotkey;
            form.screenshot_hotkey_edited = screenshot_hotkey_edited;
            form.cfg.actions.screenshot.capture_mode = screenshot_capture_mode;
            form.screenshot_reset_targets = screenshot_reset_targets;
            form.ocr_clipboard_key = ocr_clipboard_key;
            form.cfg.actions.search.hotkey = resolved_search_key(h, &SEARCH_CAPTURED,
                template.cfg.actions.search.hotkey.as_deref());
            form.cfg.actions.search.sentence_hotkey = resolved_search_key(h, &SENTENCE_SEARCH_CAPTURED,
                template.cfg.actions.search.sentence_hotkey.as_deref());
            let open_sentence_search = checked(ID_OCR_SENTENCE_SEARCH);
            if let Some(action) = &mut form.cfg.actions.ocr_clipboard {
                action.open_sentence_search = open_sentence_search;
            } else if open_sentence_search {
                form.cfg.actions.ocr_clipboard = Some(crate::config::OcrClipboardConfig {
                    open_sentence_search, ..Default::default()
                });
            }
            form.cfg.popup.sub_popups = checked(ID_SUB_POPUPS);
            form.cfg.anki.show_static_overlay = checked(ID_SHOW_STATIC_OVERLAY);
            form.cfg.anki.include_dictionary_name = checked(ID_INCLUDE_DICTIONARY_NAME);
            form.cfg.anki.first_dict_only = checked(ID_FIRST_DICT_ONLY);
            form.cfg.anki.selection_buttons = selection_buttons;
            form.cfg.anki.selection_separator = selection_separator;
            form.cfg.anki.triple_click = triple_click;
            form.cfg.plugins.enabled = self
                .plugin_names
                .iter()
                .enumerate()
                .filter(|&(idx, _)| checked(ID_PLUGIN_ENABLE_BASE + idx as i32))
                .map(|(_, name)| name.clone())
                .collect();
            form
        }
    }
}

/// Fills a combo with `values` and selects `current`.
unsafe fn fill_numeric(combo: HWND, values: &[i64], current: i64) {
    // SAFETY: the caller creates `combo`, so it is a live control.
    unsafe {
        for (i, v) in values.iter().enumerate() {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(wide(&v.to_string()).as_ptr() as isize)),
            );
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
        ANKI_MODEL_CHANGED.with(|c| {
            if c.get().is_some_and(|h| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        LANG_CHANGED.with(|c| {
            if c.get().is_some_and(|h| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CONDITION_CHANGED.with(|c| {
            if c.get().is_some_and(|h| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CONDITIONAL_TABS.with(|c| {
            let mut slot = c.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(h, _)| *h == self.hwnd.0 as isize)
            {
                *slot = None;
            }
        });
        for cell in [&SEARCH_CAPTURED, &SENTENCE_SEARCH_CAPTURED] {
            cell.with(|cell| {
                if cell.borrow().as_ref().is_some_and(|(owner, _)| *owner == self.hwnd.0 as isize) {
                    *cell.borrow_mut() = None;
                }
            });
        }
        SEARCH_REQUEST.with(|cell| {
            if cell.get().is_some_and(|(owner, _)| owner == self.hwnd.0 as isize) { cell.set(None); }
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
        SCREENSHOT_CAPTURED_VK.with(|c| {
            if c.get().is_some_and(|(h, _)| h == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        CAPTURE_PREV.with(|c| {
            let mut slot = c.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(h, _)| *h == self.hwnd.0 as isize)
            {
                *slot = None;
            }
        });

        PLUGIN_DIRS.with(|c| {
            let mut slot = c.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(h, _)| *h == self.hwnd.0 as isize)
            {
                *slot = None;
            }
        });
        for slot in [&RESIZED, &SHOW_LOGS, &USER_EDIT, &EDIT_TRACKING] {
            slot.with(|cell| {
                if cell.get() == Some(self.hwnd.0 as isize) {
                    cell.set(None);
                }
            });
        }
        WINDOW_DPI.with(|slot| {
            if slot.get().is_some_and(|(owner, _)| owner == self.hwnd.0 as isize) {
                slot.set(None);
            }
        });
        // The user can destroy a window during a drag. The operating system
        // releases capture, so this code clears the row that the drag stored.
        // A later button-up event cannot find that row.
        DRAG.with(|c| {
            if c.get().is_some_and(|d| d.window == self.hwnd.0 as isize) {
                c.set(None);
            }
        });
        // SAFETY: `SettingsWindow` owns the window and destroys it once. The font
        // outlives every control because this code destroys the window and
        // its children before it deletes the font.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if let Some(f) = self.font.get() {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

/// Creates Terms rows for one OCR language.
///
/// `list` is that language's `per_language` entry. It names dictionaries in
/// search priority order. Named rows come first. They are checked and use list
/// order. Other installed names follow and remain unchecked.
/// `per_language` stores Terms data only
/// (ARCHITECTURE.md#dictionary-and-lookup), so this function serves the Terms
/// section.
fn scope_rows(all: &[String], list: &[String], unreadable: &[String]) -> Vec<DictRow> {
    let readable = |n: &String| !unreadable.iter().any(|u| u == n);
    // A list that names nothing installed belongs to some other library.
    // That list does not ask this code to hide every dictionary.
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

    #[test]
    fn pure_search_chords_round_trip_and_preserve_unedited_values() {
        let chord = search_chord(0x46, true, true, false, false);
        assert_eq!(chord, "Ctrl+Shift+F");
        assert_eq!(crate::config::parse_hotkey(&chord),
            crate::config::parse_hotkey("Ctrl+Shift+0x46"));
        let hwnd = HWND(0xE001usize as *mut std::ffi::c_void);
        SEARCH_CAPTURED.with(|cell| *cell.borrow_mut() = None);
        assert_eq!(resolved_search_key(hwnd, &SEARCH_CAPTURED, Some("Ctrl+Shift+F")),
            Some("Ctrl+Shift+F".into()));
        SEARCH_CAPTURED.with(|cell| *cell.borrow_mut() = Some((hwnd.0 as isize, String::new())));
        assert_eq!(resolved_search_key(hwnd, &SEARCH_CAPTURED, Some("Ctrl+Shift+F")), None);
        assert_eq!(resolved_search_key(HWND::default(), &SEARCH_CAPTURED, Some("Alt+F6")), Some("Alt+F6".into()));
        SEARCH_CAPTURED.with(|cell| *cell.borrow_mut() = None);
    }

    #[test]
    fn native_search_launch_requests_and_ocr_flag_survive_shortcut_clear() {
        let mut cfg = crate::config::Config::default();
        cfg.actions.search.hotkey = Some("Ctrl+Shift+F".into());
        cfg.actions.search.sentence_hotkey = Some("Ctrl+Shift+G".into());
        cfg.actions.ocr_clipboard = Some(crate::config::OcrClipboardConfig {
            hotkey: Some("F9".into()), hotkey_linux: None, open_sentence_search: true,
        });
        let form = crate::settings::from_config(&cfg, &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        for (id, mode) in [(ID_OPEN_DICTIONARY_SEARCH, chibipop::search::SearchMode::Dictionary),
            (ID_OPEN_SENTENCE_SEARCH, chibipop::search::SearchMode::Sentence)] {
            send_command(&window, id);
            assert_eq!(window.take_search_request(), Some(mode));
            assert_eq!(window.take_search_request(), None);
        }
        send_command(&window, ID_OCR_CLIPBOARD_KEY_CLEAR);
        let edited = window.read(&form);
        let pending = crate::settings::apply_to(&edited, &cfg);
        assert_eq!(pending.actions.search.hotkey, cfg.actions.search.hotkey);
        assert_eq!(pending.actions.search.sentence_hotkey, cfg.actions.search.sentence_hotkey);
        assert!(pending.actions.ocr_clipboard.as_ref().unwrap().open_sentence_search);
        assert!(pending.actions.ocr_clipboard.as_ref().unwrap().hotkey.is_none());
        send_command(&window, ID_SEARCH_KEY_CLEAR);
        send_command(&window, ID_SENTENCE_SEARCH_KEY_CLEAR);
        let edited = window.read(&form);
        assert!(edited.cfg.actions.search.hotkey.is_none());
        assert!(edited.cfg.actions.search.sentence_hotkey.is_none());
    }

    fn remove_layout_entry(layout: &mut SettingsLayout, id: SettingId) -> EntrySpec {
        for tab in &mut layout.tabs {
            for section in &mut tab.sections {
                if let Some(index) = section.entries.iter().position(|entry| entry.id == id) {
                    return section.entries.remove(index);
                }
            }
        }
        panic!("missing layout entry {id:?}");
    }

    fn move_layout_entry(
        layout: &mut SettingsLayout,
        id: SettingId,
        tab: usize,
        section: usize,
        index: usize,
    ) {
        let entry = remove_layout_entry(layout, id);
        layout.tabs[tab].sections[section].entries.insert(index, entry);
    }

    fn control_top(window: &SettingsWindow, id: i32) -> i32 {
        // SAFETY: The test window owns the requested control.
        unsafe {
            let control = dlg_item(window.hwnd, id).expect("control should exist");
            let mut rect = RECT::default();
            GetWindowRect(control, &mut rect).expect("control rectangle");
            let mut point = POINT { x: rect.left, y: rect.top };
            assert!(ScreenToClient(window.content, &mut point).as_bool());
            point.y
        }
    }

    fn select_sentence_mode(window: &SettingsWindow, mode: SentenceMode) {
        let index = SENTENCE_MODES
            .iter()
            .position(|(candidate, _)| *candidate == mode)
            .expect("sentence mode should exist");
        // SAFETY: The test window owns the sentence combo.
        unsafe {
            let combo = dlg_item(window.hwnd, ID_SENTENCE_MODE).expect("sentence combo");
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), None);
            let command = ID_SENTENCE_MODE as usize | ((CBN_SELCHANGE as usize) << 16);
            SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(command)), None);
        }
        window.pump(|| {});
    }

    fn send_command(window: &SettingsWindow, id: i32) {
        // SAFETY: The command targets a control owned by this test window.
        unsafe {
            SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(id as usize)), None);
        }
    }

    fn visible_tabstop_ids(window: &SettingsWindow) -> Vec<i32> {
        let mut ids = Vec::new();
        // SAFETY: The loop follows live content-pane siblings.
        unsafe {
            let mut next = GetWindow(window.content, GW_CHILD);
            while let Ok(control) = next {
                let style = GetWindowLongW(control, GWL_STYLE) as u32;
                if style & WS_TABSTOP.0 != 0 && IsWindowVisible(control).as_bool() {
                    ids.push(GetDlgCtrlID(control));
                }
                next = GetWindow(control, GW_HWNDNEXT);
            }
        }
        ids
    }

    fn nondefault_form() -> SettingsForm {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.cfg.trigger.mode = crate::config::TriggerMode::Toggle;
        form.cfg.trigger.trigger_key = "f6".into();
        form.cfg.trigger.per_character_lookup = true;
        form.cfg.popup.theme = "light".into();
        form.cfg.popup.exclude_from_capture = true;
        form.cfg.popup.max_width_percent = 35;
        form.cfg.popup.max_height_percent = 55;
        form.cfg.popup.summary_chars = 70;
        form.cfg.popup.font = "Akari test font".into();
        form.cfg.popup.highlight_match = false;
        form.cfg.popup.scroll_popup = false;
        form.cfg.popup.edge_autoscroll = false;
        form.cfg.popup.side_panel = true;
        form.cfg.popup.layout_mode = LayoutMode::Compact;
        form.cfg.popup.dictionary_styling = false;
        form.cfg.popup.show_examples = false;
        form.cfg.popup.show_attributions = false;
        form.cfg.popup.show_images = false;
        form.cfg.popup.show_part_of_speech = true;
        form.cfg.dictionaries.ranking_strategy = RankingStrategy::Median;
        form.cfg.ocr.max_ocr_passes = 2;
        form.cfg.ocr.prefer_vertical = true;
        form.cfg.ocr.capture_width = 333;
        form.cfg.ocr.capture_height = 222;
        form.cfg.ocr.scan_alphanumeric = false;
        form.cfg.ocr.discard_furigana = false;
        form.cfg.ocr.language = "zz-ZZ".into();
        form.dict_list_language = "zz-ZZ".into();
        form.cfg.ocr.engine = "missing-provider".into();
        form.cfg.debug.show_scan_region = true;
        form.cfg.debug.show_lookup_log = true;
        form.cfg.debug.show_engine_log = true;
        form.cfg.debug.show_adapter_log = true;
        form.cfg.anki.enabled = true;
        form.cfg.anki.url = "http://127.0.0.1:9999".into();
        form.cfg.anki.deck = "Akari deck".into();
        form.cfg.anki.model = "Akari model".into();
        form.cfg.anki.add_key = "f2".into();
        form.cfg.anki.notify_on_add = false;
        form.cfg.anki.sentence_mode = SentenceMode::Static;
        form.cfg.anki.static_region_key = "f3".into();
        form.cfg.anki.show_static_overlay = false;
        form.cfg.anki.include_dictionary_name = false;
        form.cfg.anki.first_dict_only = true;
        form.cfg.anki.selection_buttons = SelectionButtons::PrimaryReplacing;
        form.cfg.anki.selection_separator = SelectionSeparator::LineBreak;
        form.cfg.anki.triple_click = TripleClick::Line;
        form.cfg.actions.screenshot.hotkey = "f4".into();
        form.cfg.actions.screenshot.hotkey_linux = Some("SUPER+S".into());
        form.cfg.actions.screenshot.include_on_add = true;
        form.cfg.actions.screenshot.capture_mode = ScreenshotMode::FixedRegion;
        form.cfg.actions.screenshot.fixed_region = Some([10, 20, 300, 200]);
        form.terms = vec![DictRow { name: "Terms".into(), enabled: false }];
        form.frequency = vec![DictRow { name: "Frequency".into(), enabled: true }];
        form.pitch = vec![DictRow { name: "Pitch".into(), enabled: false }];
        form.field_map = Some(vec![crate::config::FieldMapping {
            anki_field: "Front".into(),
            source: "expression".into(),
        }]);
        form.ocr_clipboard_key = Some("f5".into());
        form
    }

    #[test]
    fn runtime_tabs_follow_layout_and_queue_the_initial_selection() {
        let mut layout = SettingsLayout::embedded().unwrap();
        layout.tabs.swap(0, 1);
        layout.tabs[1].sections.swap(0, 1);
        move_layout_entry(&mut layout, SettingId::AnkiFieldMap, 0, 0, 0);
        layout.validate().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();

        assert_eq!(7, window.tab_count());
        assert_eq!(Some("Shortcuts"), window.tab_label(0));
        assert_eq!(Some(TabId::Shortcuts), window.tab_id(0));
        assert_eq!(Some(0), window.field_map_tab());
        assert!(window.tab_needs_anki_detection(0));
        assert_eq!(Some(0), window.take_tab_change());

        window.switch_tab(1);
        assert!(control_top(&window, ID_MAX_WIDTH) < control_top(&window, ID_THEME));
        let audit = crate::ui::audit::dump(window.hwnd);
        let ring = audit["tab_ring"].as_array().unwrap();
        let width_index = ring.iter().position(|id| id.as_i64() == Some(i64::from(ID_MAX_WIDTH))).unwrap();
        let theme_index = ring.iter().position(|id| id.as_i64() == Some(i64::from(ID_THEME))).unwrap();
        assert!(width_index < theme_index);

        window.switch_tab(3);
        // SAFETY: The tab control belongs to the live test window.
        let selected = unsafe {
            let tab = dlg_item(window.hwnd, ID_TAB).unwrap();
            SendMessageW(tab, TCM_GETCURSEL_MSG, None, None).0
        };
        assert_eq!(3, selected);
    }

    #[test]
    fn native_window_resizes_maximizes_and_tracks_a_minimum() {
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        // SAFETY: The root window remains live for this test.
        unsafe {
            let style = WINDOW_STYLE(GetWindowLongW(window.hwnd, GWL_STYLE) as u32);
            assert!(style.contains(WS_THICKFRAME));
            assert!(style.contains(WS_MAXIMIZEBOX));
            let mut limits = MINMAXINFO::default();
            let _ = wndproc(
                window.hwnd,
                WM_GETMINMAXINFO,
                WPARAM(0),
                LPARAM(&mut limits as *mut _ as isize),
            );
            assert!(limits.ptMinTrackSize.x >= dpi_scale(window.hwnd, MIN_CLIENT_W));
            assert!(limits.ptMinTrackSize.y >= dpi_scale(window.hwnd, MIN_CLIENT_H));
        }

        resize_client(&window, 700, 560);
        let restored = outer_rect(&window);
        // SAFETY: The test owns the native root window.
        unsafe {
            let _ = ShowWindow(window.hwnd, SW_MAXIMIZE);
            window.pump(|| {});
            assert!(IsZoomed(window.hwnd).as_bool());
            let _ = ShowWindow(window.hwnd, SW_RESTORE);
            window.pump(|| {});
        }
        let after = outer_rect(&window);
        assert_eq!(restored.right - restored.left, after.right - after.left);
        assert_eq!(restored.bottom - restored.top, after.bottom - after.top);
    }

    struct TestDpiContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

    impl TestDpiContext {
        fn per_monitor() -> Self {
            use windows::Win32::UI::HiDpi::{
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            };
            // SAFETY: Only this test thread changes awareness; Drop restores it after all windows close.
            let previous = unsafe {
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            };
            assert!(!previous.0.is_null());
            Self(previous)
        }
    }

    impl Drop for TestDpiContext {
        fn drop(&mut self) {
            // SAFETY: This is the previous valid context returned on the same thread.
            unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.0); }
        }
    }

    fn assert_native_edges(window: &SettingsWindow) {
        let width = client_w(window.hwnd);
        let height = client_h(window.hwnd);
        for id in [ID_TAB, ID_UPDATES, ID_CHECK_UPDATE, ID_APPLY_STATE,
            ID_RUNTIME_STATUS, ID_STATUS, ID_APPLY, ID_QUIT] {
            let rect = control_rect(window, id, window.hwnd);
            assert!(rect.left >= 0 && rect.right <= width, "id {id}: {rect:?}, width {width}");
            assert!(rect.top >= 0 && rect.bottom <= height, "id {id}: {rect:?}, height {height}");
            assert!(rect.bottom > rect.top, "zero-height control {id}");
        }
        assert_eq!(width, client_w(window.viewport));
        assert_eq!(width, client_w(window.content));
        let viewport = control_rect(window, ID_VIEWPORT, window.hwnd);
        let footer = control_rect(window, ID_UPDATES, window.hwnd);
        assert!(viewport.bottom <= footer.top);
        // SAFETY: These runtime handles remain live throughout the test.
        unsafe {
            for tab in &window.tabs {
                for section in &tab.sections {
                    for control in section.entries.iter().flat_map(|entry| &entry.controls)
                        .map(|control| control.hwnd).chain(std::iter::once(section.frame))
                    {
                        let mut rect = RECT::default();
                        GetWindowRect(control, &mut rect).unwrap();
                        let mut point = POINT { x: rect.right, y: rect.top };
                        assert!(ScreenToClient(window.content, &mut point).as_bool());
                        assert!(point.x <= width, "control {:?} exceeds {width}: {}", control, point.x);
                    }
                }
            }
        }
    }

    #[test]
    fn native_minimum_and_fitted_sizes_reserve_scrollbar_and_client_edges() {
        let _awareness = TestDpiContext::per_monitor();
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        // SAFETY: Native DPI is read independently of the settings geometry helper.
        let dpi = unsafe { GetDpiForWindow(window.hwnd) } as i32;
        for (width, height) in [(520, 430), (560, 480), (760, 520)] {
            resize_client(&window, width, height);
            assert_eq!(width * dpi / 96, client_w(window.hwnd));
            assert_eq!(height * dpi / 96, client_h(window.hwnd));
            assert_native_edges(&window);
            // SAFETY: The native combos and output rectangles remain live during each synchronous message.
            unsafe {
                for id in [ID_FONT, ID_OCR_LANG, ID_ANKI_DECK] {
                    let mut dropped = RECT::default();
                    assert_ne!(0, SendMessageW(dlg_item(window.hwnd, id).unwrap(),
                        CB_GETDROPPEDCONTROLRECT, None,
                        Some(LPARAM(&mut dropped as *mut _ as isize))).0);
                    assert!(dropped.bottom - dropped.top >= 100 * dpi / 96,
                        "combo {id} lost its drop-down height at client width {width}");
                }
            }
            for tab in 0..window.tab_count() {
                window.switch_tab(tab);
                assert_eq!(width * dpi / 96, client_w(window.hwnd));
            }
        }
        // SAFETY: The test supplies live, writable message storage.
        unsafe {
            let mut limits = MINMAXINFO::default();
            SendMessageW(window.hwnd, WM_GETMINMAXINFO, None,
                Some(LPARAM(&mut limits as *mut _ as isize)));
            SetWindowPos(window.hwnd, None, 0, 0,
                limits.ptMinTrackSize.x, limits.ptMinTrackSize.y,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE).unwrap();
        }
        window.pump(|| {});
        assert_eq!(MIN_CLIENT_W * dpi / 96, client_w(window.hwnd));
        assert_eq!(MIN_CLIENT_H * dpi / 96, client_h(window.hwnd));
        assert_native_edges(&window);
        assert_eq!(form, window.read(&form));
    }

    #[test]
    fn resize_preserves_scroll_and_keeps_lower_focus_visible() {
        let form = nondefault_form();
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::ScreenshotTargets, 0, 0, 0);
        let window = SettingsWindow::open_with_layout(&form, &[], ApplyMode::Standalone, layout).unwrap();
        resize_client(&window, 700, 460);
        let order = visible_tabstop_ids(&window);
        // SAFETY: This thread owns the root and focusable descendants.
        unsafe { SetFocus(Some(window.hwnd)).unwrap(); }
        scroll_to(window.hwnd, |_| 180);
        assert_eq!(180, scroll_position(window.hwnd));
        resize_client(&window, 620, 480);
        assert_eq!(180, scroll_position(window.hwnd));
        let focus_top = control_top(&window, ID_SHOW_POS);
        scroll_to(window.hwnd, |_| focus_top);
        // SAFETY: The lower checkbox is visible on the active Popup tab.
        unsafe { SetFocus(Some(dlg_item(window.hwnd, ID_SHOW_POS).unwrap())).unwrap(); }
        resize_client(&window, 520, 430);
        let focus = control_rect(&window, ID_SHOW_POS, window.viewport);
        assert!(focus.top >= 0 && focus.bottom <= client_h(window.viewport), "{focus:?}");
        assert!(scroll_position(window.hwnd) > 0);
        assert_eq!(order, visible_tabstop_ids(&window));
        let before = scroll_position(window.hwnd);
        let size = outer_rect(&window);
        window.refresh_screenshot_targets(&form.cfg.actions.screenshot);
        assert_eq!(before, scroll_position(window.hwnd));
        assert_eq!(size, outer_rect(&window));
        let mut screenshot = form.cfg.actions.screenshot.clone();
        screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "test-window".into(),
            title: "A long game window title ".repeat(20),
        });
        window.refresh_screenshot_targets(&screenshot);
        let focus = control_rect(&window, ID_SHOW_POS, window.viewport);
        assert!(focus.top >= 0 && focus.bottom <= client_h(window.viewport), "{focus:?}");
        assert!(scroll_position(window.hwnd) > before);
        assert_eq!(size, outer_rect(&window));
        // SAFETY: Resize and dynamic reflow keep the same focused child.
        unsafe { assert_eq!(dlg_item(window.hwnd, ID_SHOW_POS).unwrap(), GetFocus()); }
        window.switch_tab(1);
        assert_eq!(0, scroll_position(window.hwnd));
    }

    #[test]
    fn dynamic_reflow_clamps_scroll_when_content_shrinks() {
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        resize_client(&window, 620, 480);
        window.switch_tab(window.field_map_tab().unwrap());
        window.populate_fields((0..20).map(|index| format!("Field {index}")).collect());
        while window.pending_field_map.borrow().is_some() { window.pump(|| {}); }
        window.toggle_field_map();
        // SAFETY: Moving focus to the root allows the scroll offset to be measured independently.
        unsafe { SetFocus(Some(window.hwnd)).unwrap(); }
        scroll_to(window.hwnd, |_| i32::MAX);
        let before = scroll_position(window.hwnd);
        let size = outer_rect(&window);
        window.toggle_field_map();
        let maximum = (dpi_scale(window.hwnd, window.tab_page_h(window.current_tab.get()))
            - client_h(window.viewport)).max(0);
        assert!(before > maximum);
        assert_eq!(maximum, scroll_position(window.hwnd));
        assert_eq!(size, outer_rect(&window));
        // SAFETY: The content pane is a live child of the viewport.
        unsafe {
            let mut rect = RECT::default();
            GetWindowRect(window.content, &mut rect).unwrap();
            let mut origin = POINT { x: rect.left, y: rect.top };
            assert!(ScreenToClient(window.viewport, &mut origin).as_bool());
            assert_eq!(-maximum, origin.y);
        }
    }

    #[test]
    fn dpi_messages_update_font_geometry_and_dropdown_capacity() {
        use windows::Win32::Graphics::Gdi::GetObjectW;
        let _awareness = TestDpiContext::per_monitor();
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        let mut font_heights = Vec::new();
        for dpi in [96u32, 120, 144, 96] {
            let rect = RECT { left: 30, top: 30, right: 1030, bottom: 750 };
            // SAFETY: Synchronous delivery borrows the suggested rectangle for this call only.
            unsafe {
                SendMessageW(window.hwnd, WM_DPICHANGED,
                    Some(WPARAM(dpi as usize | ((dpi as usize) << 16))),
                    Some(LPARAM(&rect as *const _ as isize)));
            }
            window.pump(|| {});
            assert_eq!(rect, outer_rect(&window));
            assert_eq!(dpi, window.font_dpi.get());
            assert_native_edges(&window);
            // SAFETY: All control and font handles remain owned by this window.
            unsafe {
                let font = window.font.get().unwrap();
                let mut logfont = LOGFONTW::default();
                assert_ne!(0, GetObjectW(font.into(), std::mem::size_of::<LOGFONTW>() as i32,
                    Some(&mut logfont as *mut _ as *mut core::ffi::c_void)));
                font_heights.push(logfont.lfHeight.abs());
                for id in [ID_THEME, ID_CAPTURE_W, ID_STATUS, ID_TAB, ID_RUNTIME_STATUS] {
                    let control = dlg_item(window.hwnd, id).unwrap();
                    assert_eq!(font.0 as isize, SendMessageW(control, WM_GETFONT, None, None).0);
                }
                for id in [ID_THEME, ID_FONT, ID_OCR_LANG, ID_MAX_WIDTH, ID_ANKI_DECK] {
                    let combo = dlg_item(window.hwnd, id).unwrap();
                    let mut dropped = RECT::default();
                    assert_ne!(0, SendMessageW(combo, CB_GETDROPPEDCONTROLRECT, None,
                        Some(LPARAM(&mut dropped as *mut _ as isize))).0);
                    assert!(dropped.bottom - dropped.top >= (100 * dpi / 96) as i32,
                        "collapsed combo {id} at {dpi} DPI: {dropped:?}");
                }
            }
            assert_eq!(dpi_scale(window.hwnd, TAB_H),
                client_h(dlg_item_for_test(&window, ID_TAB)));
            assert_eq!(form, window.read(&form));
        }
        assert!(font_heights[0] < font_heights[1]);
        assert!(font_heights[1] < font_heights[2]);
        assert_eq!(font_heights[0], font_heights[3]);
    }

    fn dlg_item_for_test(window: &SettingsWindow, id: i32) -> HWND {
        // SAFETY: The test owns the requested child window.
        unsafe { dlg_item(window.hwnd, id).unwrap() }
    }

    #[test]
    fn native_width_and_height_reflow_preserves_values() {
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        resize_client(&window, MIN_CLIENT_W, MIN_CLIENT_H);
        let narrow_field = control_rect(&window, ID_THEME, window.content);
        let narrow_status = control_rect(&window, ID_STATUS, window.hwnd);
        let narrow_viewport = control_rect(&window, ID_VIEWPORT, window.hwnd);
        let narrow_apply = control_rect(&window, ID_APPLY, window.hwnd);
        let narrow_footer = control_rect(&window, ID_UPDATES, window.hwnd);
        assert!(narrow_viewport.bottom <= narrow_footer.top);
        assert!(narrow_status.bottom <= client_h(window.hwnd));

        resize_client(&window, 760, 680);
        let wide_field = control_rect(&window, ID_THEME, window.content);
        let wide_status = control_rect(&window, ID_STATUS, window.hwnd);
        let tall_viewport = control_rect(&window, ID_VIEWPORT, window.hwnd);
        let wide_apply = control_rect(&window, ID_APPLY, window.hwnd);
        assert!(wide_field.right - wide_field.left > narrow_field.right - narrow_field.left);
        assert!(wide_status.right - wide_status.left > narrow_status.right - narrow_status.left);
        assert!(tall_viewport.bottom - tall_viewport.top
            > narrow_viewport.bottom - narrow_viewport.top);
        assert!(wide_apply.left > narrow_apply.left);

        window.populate_fields((0..4).map(|index| format!("Field {index}")).collect());
        window.toggle_field_map();
        let narrow_combo = {
            resize_client(&window, MIN_CLIENT_W, 600);
            control_rect(&window, ID_FIELD_MAP_BASE, window.content)
        };
        let narrow_second = control_rect(&window, ID_FIELD_MAP_BASE + 2, window.content);
        assert!(narrow_combo.right < narrow_second.left);
        let wide_combo = {
            resize_client(&window, 760, 600);
            control_rect(&window, ID_FIELD_MAP_BASE, window.content)
        };
        let wide_second = control_rect(&window, ID_FIELD_MAP_BASE + 2, window.content);
        assert!(wide_combo.right < wide_second.left);
        assert!(wide_combo.right - wide_combo.left > narrow_combo.right - narrow_combo.left);
        assert_eq!(form, window.read(&form));
    }

    #[test]
    fn dynamic_reflow_preserves_user_size_and_maximized_state() {
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Live).unwrap();
        resize_client(&window, 720, 560);
        let before = outer_rect(&window);
        let mut screenshot = form.cfg.actions.screenshot.clone();
        screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "test-window".into(),
            title: "A long game window title ".repeat(20),
        });
        window.refresh_screenshot_targets(&screenshot);
        window.populate_fields((0..12).map(|index| format!("Field {index}")).collect());
        window.toggle_field_map();
        let after = outer_rect(&window);
        assert_eq!(before.right - before.left, after.right - after.left);
        assert_eq!(before.bottom - before.top, after.bottom - after.top);

        // SAFETY: The test owns the native root window.
        unsafe {
            let _ = ShowWindow(window.hwnd, SW_MAXIMIZE);
            window.pump(|| {});
            assert!(IsZoomed(window.hwnd).as_bool());
            window.refresh_screenshot_targets(&screenshot);
            window.populate_fields((0..8).map(|index| format!("Field {index}")).collect());
            assert!(IsZoomed(window.hwnd).as_bool());
            let _ = ShowWindow(window.hwnd, SW_RESTORE);
            window.pump(|| {});
        }
    }

    #[test]
    fn footer_keeps_apply_runtime_and_operation_state_separate() {
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        // SAFETY: Footer controls remain live for this test.
        unsafe {
            assert_eq!("Apply: Loaded", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
            assert_eq!(
                "Scanning is inactive.",
                window_text(dlg_item(window.hwnd, ID_RUNTIME_STATUS).unwrap()),
            );
        }
        window.set_apply_state(ApplyState::Applied);
        window.set_runtime_status("ja-JP", "builtin fallback", true);
        window.set_status("Saved configuration.");
        window.set_capture_fields(&form.cfg.ocr);
        window.populate_combos(&[], &[], Vec::new());
        window.switch_tab(1);
        window.pump(|| {});
        // SAFETY: Footer controls remain live for this test.
        unsafe {
            assert_eq!("Apply: Applied", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
            assert_eq!(
                "Language: ja-JP | OCR: builtin fallback | Anki: enabled",
                window_text(dlg_item(window.hwnd, ID_RUNTIME_STATUS).unwrap()),
            );
            assert_eq!(
                "Saved configuration.",
                window_text(dlg_item(window.hwnd, ID_STATUS).unwrap()),
            );
        }

        send_command(&window, ID_SHOW_SCAN);
        window.pump(|| {});
        // SAFETY: The Apply state control remains live.
        unsafe {
            assert_eq!("Apply: Pending", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
        }
        window.set_apply_state(ApplyState::Applying);
        send_command(&window, ID_SHOW_SCAN);
        window.pump(|| {});
        // SAFETY: The Apply state control remains live.
        unsafe {
            assert_eq!("Apply: Applying", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
        }
        window.set_apply_state(ApplyState::Applied);
        // SAFETY: The control remains live.
        unsafe {
            assert_eq!("Apply: Pending", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
        }
        window.set_apply_state(ApplyState::Applying);
        window.set_apply_state(ApplyState::Failed);
        // SAFETY: The Apply state control remains live.
        unsafe {
            assert_eq!("Apply: Failed", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
        }
    }

    #[test]
    fn debug_action_is_separate_from_dirty_settings() {
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        assert_eq!(7, window.tab_count());
        assert_eq!(Some("Debug"), window.tab_label(6));
        window.switch_tab(6);
        send_command(&window, ID_SHOW_LIVE_LOGS);
        window.pump(|| {});
        assert!(window.take_show_logs());
        assert!(!window.take_show_logs());
        // SAFETY: The Apply state control remains live.
        unsafe {
            assert_eq!("Apply: Loaded", window_text(dlg_item(window.hwnd, ID_APPLY_STATE).unwrap()));
        }
    }

    #[test]
    fn reordered_entries_set_positions_and_radio_group_boundaries() {
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::LookupMode, 0, 0, 0);
        layout.validate().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();
        window.switch_tab(0);

        assert!(window.entry_top(SettingId::LookupMode) < window.entry_top(SettingId::PopupTheme));
        // SAFETY: These controls belong to one live dialog group.
        unsafe {
            let press = dlg_item(window.hwnd, ID_MODE_PRESS).unwrap();
            let next = GetNextDlgGroupItem(window.content, Some(press), false).unwrap();
            let next_id = GetDlgCtrlID(next);
            assert!(
                [ID_MODE_LIVE, ID_MODE_HOLD, ID_MODE_TOGGLE, ID_MODE_PRESS]
                    .contains(&next_id),
                "radio group escaped to control {next_id}",
            );
            assert_ne!(dlg_item(window.hwnd, ID_THEME).unwrap(), next);
        }
    }

    #[test]
    fn cancelling_partial_field_rows_reflows_the_retained_controls() {
        check_cancelled_field_rows(false);
        check_cancelled_field_rows(true);
    }

    fn control_rect(window: &SettingsWindow, id: i32, parent: HWND) -> RECT {
        // SAFETY: The test window owns the requested control.
        unsafe {
            let control = dlg_item(window.hwnd, id).expect("control should exist");
            let mut rect = RECT::default();
            GetWindowRect(control, &mut rect).expect("control rectangle");
            let mut top_left = POINT {
                x: rect.left,
                y: rect.top,
            };
            let mut bottom_right = POINT {
                x: rect.right,
                y: rect.bottom,
            };
            assert!(ScreenToClient(parent, &mut top_left).as_bool());
            assert!(ScreenToClient(parent, &mut bottom_right).as_bool());
            RECT {
                left: top_left.x,
                top: top_left.y,
                right: bottom_right.x,
                bottom: bottom_right.y,
            }
        }
    }

    fn outer_rect(window: &SettingsWindow) -> RECT {
        // SAFETY: The root window remains live for this test.
        unsafe {
            let mut rect = RECT::default();
            GetWindowRect(window.hwnd, &mut rect).expect("window rectangle");
            rect
        }
    }

    fn resize_client(window: &SettingsWindow, width: i32, height: i32) {
        window.fit_to(width, height);
        window.pump(|| {});
    }

    #[test]
    fn live_screenshot_targets_resize_and_reflow_their_entry() {
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::ScreenshotTargets, 0, 0, 0);
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(&form, &[], ApplyMode::Standalone, layout).unwrap();
        let before = control_top(&window, ID_THEME);
        // SAFETY: The captured test window owns the summary for this closure's lifetime.
        let summary_height = || unsafe {
            let control = dlg_item(window.hwnd, ID_SCREENSHOT_SUMMARY).unwrap();
            let mut rect = RECT::default();
            GetWindowRect(control, &mut rect).unwrap();
            rect.bottom - rect.top
        };
        let initial_height = summary_height();
        let mut screenshot = form.cfg.actions.screenshot.clone();
        screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "test-window".into(),
            title: "A long game window title ".repeat(20),
        });
        window.refresh_screenshot_targets(&screenshot);
        assert!(summary_height() > initial_height);
        assert!(control_top(&window, ID_THEME) > before, "updated summary still has its initial height");
        resize_client(&window, 760, 600);
        let wide_height = summary_height();
        resize_client(&window, MIN_CLIENT_W, 600);
        assert!(summary_height() > wide_height);
        resize_client(&window, WIN_W, 600);
        send_command(&window, ID_SCREENSHOT_RESET);
        window.pump(|| {});
        assert_eq!(initial_height, summary_height());
        assert_eq!(before, control_top(&window, ID_THEME));
    }

    fn check_cancelled_field_rows(empty_result: bool) {
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::AnkiFieldMap, 0, 0, 0);
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(&form, &[], ApplyMode::Standalone, layout).unwrap();
        window.toggle_field_map();
        let fields = (0..30).map(|index| format!("Field {index}")).collect();
        window.populate_fields(fields);
        assert!(window.pending_field_map.borrow().is_some());
        let before = control_top(&window, ID_THEME);
        let retained: Vec<_> = window.field_map_rows.borrow().iter().map(|(name, _)| name.clone()).collect();
        let first_combo = window.field_map_rows.borrow()[0].1;
        // SAFETY: The test owns this live field combo.
        unsafe { SendMessageW(first_combo, CB_SETCURSEL, Some(WPARAM(1)), None); }
        window.populate_fields(if empty_result { Vec::new() } else { retained.clone() });
        assert!(window.pending_field_map.borrow().is_none());
        assert!(control_top(&window, ID_THEME) < before, "cancelled rows still reserve the old height");
        assert_eq!(first_combo, window.field_map_rows.borrow()[0].1);
        // SAFETY: Repacking must retain the same live combo and its value.
        assert_eq!(1, unsafe { SendMessageW(first_combo, CB_GETCURSEL, None, None).0 });
        for index in 0..retained.len() {
            assert!(control_top(&window, ID_FIELD_MAP_BASE + index as i32) + dpi_scale(window.hwnd, ROW_H)
                <= control_top(&window, ID_THEME), "retained row overlaps the following entry");
        }
        window.toggle_field_map();
        window.toggle_field_map();
        for index in 0..retained.len() {
            assert!(control_top(&window, ID_FIELD_MAP_BASE + index as i32) + dpi_scale(window.hwnd, ROW_H)
                <= control_top(&window, ID_THEME));
        }
    }

    #[test]
    fn moved_field_map_reflows_following_entries_for_expand_and_chunks() {
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::AnkiFieldMap, 0, 0, 0);
        layout.validate().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();
        window.switch_tab(0);
        let collapsed_top = control_top(&window, ID_THEME);

        window.populate_fields((0..6).map(|index| format!("Field {index}")).collect());
        // SAFETY: The first chunk creates identifiers 200 through 203.
        unsafe {
            assert!(!IsWindowVisible(dlg_item(window.hwnd, ID_FIELD_MAP_BASE).unwrap()).as_bool());
        }
        window.toggle_field_map();
        let expanded_top = control_top(&window, ID_THEME);
        assert!(expanded_top > collapsed_top);
        // SAFETY: Expanded rows belong to the selected owner tab.
        unsafe {
            assert!(IsWindowVisible(dlg_item(window.hwnd, ID_FIELD_MAP_BASE).unwrap()).as_bool());
            assert!(IsWindowVisible(dlg_item(window.hwnd, ID_FIELD_MAP_BASE + 1).unwrap()).as_bool());
        }
        let ring = visible_tabstop_ids(&window);
        let position = |id| ring.iter().position(|candidate| *candidate == id).unwrap();
        assert!(position(ID_FIELD_MAP_TOGGLE) < position(ID_FIELD_MAP_BASE));
        assert!(position(ID_FIELD_MAP_BASE) < position(ID_FIELD_MAP_BASE + 1));
        assert!(position(ID_FIELD_MAP_BASE + 1) < position(ID_THEME));
        window.pump(|| {});
        window.pump(|| {});
        assert_eq!(expanded_top, control_top(&window, ID_THEME));
        window.toggle_field_map();
        assert_eq!(collapsed_top, control_top(&window, ID_THEME));
    }

    #[test]
    fn conditional_entries_intersect_state_with_their_owner_tabs() {
        let mut layout = SettingsLayout::embedded().unwrap();
        move_layout_entry(&mut layout, SettingId::AnkiStaticOverlay, 0, 0, 0);
        layout.validate().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();
        window.switch_tab(0);
        let collapsed_top = control_top(&window, ID_THEME);
        // SAFETY: The overlay is hidden when sentence mode is not static.
        unsafe {
            assert!(!IsWindowVisible(dlg_item(window.hwnd, ID_SHOW_STATIC_OVERLAY).unwrap()).as_bool());
        }
        window.switch_tab(1);
        // SAFETY: The shortcut stays visible and editable.
        unsafe {
            let key = dlg_item(window.hwnd, ID_STATIC_REGION_KEY).unwrap();
            assert!(IsWindowVisible(key).as_bool());
            assert!(windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(key).as_bool());
        }

        select_sentence_mode(&window, SentenceMode::Static);
        window.switch_tab(1);
        // SAFETY: Static mode enables the discoverable shortcut.
        unsafe {
            let key = dlg_item(window.hwnd, ID_STATIC_REGION_KEY).unwrap();
            assert!(windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(key).as_bool());
        }
        window.switch_tab(0);
        // SAFETY: The overlay appears only on its owner tab.
        unsafe {
            assert!(IsWindowVisible(dlg_item(window.hwnd, ID_SHOW_STATIC_OVERLAY).unwrap()).as_bool());
        }
        assert!(control_top(&window, ID_THEME) > collapsed_top);
        window.switch_tab(4);
        // SAFETY: The owner tab is hidden.
        unsafe {
            assert!(!IsWindowVisible(dlg_item(window.hwnd, ID_SHOW_STATIC_OVERLAY).unwrap()).as_bool());
        }
        select_sentence_mode(&window, SentenceMode::Sentence);
        window.switch_tab(0);
        assert_eq!(collapsed_top, control_top(&window, ID_THEME));
    }

    #[test]
    fn duplicate_provider_names_keep_the_first_discovered_directory() {
        let first = PathBuf::from("C:/plugins/first");
        let second = PathBuf::from("C:/plugins/second");
        let directories = first_provider_directories([
            ("provider".to_string(), first.clone()),
            ("provider".to_string(), second),
        ]);
        assert_eq!(Some(&first), directories.get("provider"));
    }

    #[test]
    fn every_nondefault_setting_round_trips_through_the_reordered_window() {
        let mut layout = SettingsLayout::embedded().unwrap();
        layout.tabs.rotate_left(2);
        for tab in &mut layout.tabs {
            tab.sections.reverse();
            for section in &mut tab.sections {
                section.entries.reverse();
            }
        }
        layout.validate().unwrap();
        let form = nondefault_form();
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();

        let read = window.read(&form);
        assert_eq!(form, read);
        assert_eq!(
            crate::settings::apply_to(&form, &form.cfg),
            crate::settings::apply_to(&read, &form.cfg),
        );
    }

    #[test]
    fn optional_shortcuts_preserve_untouched_and_cancelled_values() {
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        assert_eq!(form, window.read(&form));

        for id in [
            ID_ANKI_ADD_KEY,
            ID_STATIC_REGION_KEY,
            ID_SCREENSHOT_HOTKEY,
            ID_OCR_CLIPBOARD_KEY,
        ] {
            send_command(&window, id);
            assert!(window.handle_capture_key(0x1B));
        }
        assert_eq!(form, window.read(&form));
    }

    #[test]
    fn optional_shortcuts_rebind_independently() {
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        for (id, key) in [
            (ID_ANKI_ADD_KEY, 0x76),
            (ID_STATIC_REGION_KEY, 0x77),
            (ID_SCREENSHOT_HOTKEY, 0x78),
            (ID_OCR_CLIPBOARD_KEY, 0x79),
        ] {
            send_command(&window, id);
            assert!(window.handle_capture_key(key));
        }
        let read = window.read(&form);
        assert_eq!("f7", read.cfg.anki.add_key);
        assert_eq!("f8", read.cfg.anki.static_region_key);
        assert_eq!("f9", read.cfg.actions.screenshot.hotkey);
        assert_eq!(Some("f10"), read.ocr_clipboard_key.as_deref());
        assert!(read.screenshot_hotkey_edited);
        assert_eq!("f6", read.cfg.trigger.trigger_key);
    }

    #[test]
    fn optional_clear_cancels_active_capture_and_uses_disabled_values() {
        let form = nondefault_form();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        for (key, clear) in [
            (ID_ANKI_ADD_KEY, ID_ANKI_ADD_KEY_CLEAR),
            (ID_STATIC_REGION_KEY, ID_STATIC_REGION_KEY_CLEAR),
            (ID_SCREENSHOT_HOTKEY, ID_SCREENSHOT_KEY_CLEAR),
            (ID_OCR_CLIPBOARD_KEY, ID_OCR_CLIPBOARD_KEY_CLEAR),
        ] {
            send_command(&window, key);
            assert!(CAPTURING.with(|cell| cell.get()).is_some());
            send_command(&window, clear);
            assert!(CAPTURING.with(|cell| cell.get()).is_none());
            // SAFETY: The key control stays live after Clear.
            unsafe {
                assert_eq!("Not set", window_text(dlg_item(window.hwnd, key).unwrap()));
            }
        }
        window.switch_tab(0);
        let read = window.read(&form);
        assert!(read.cfg.anki.add_key.is_empty());
        assert!(read.cfg.anki.static_region_key.is_empty());
        assert!(read.cfg.actions.screenshot.hotkey.is_empty());
        assert!(read.ocr_clipboard_key.is_none());
        assert!(read.screenshot_hotkey_edited);
        assert_eq!("f6", read.cfg.trigger.trigger_key);
    }

    #[test]
    fn static_region_clear_works_outside_static_sentence_mode() {
        let mut form = nondefault_form();
        form.cfg.anki.sentence_mode = SentenceMode::Sentence;
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        window.switch_tab(1);
        // SAFETY: The shortcut stays enabled outside static mode.
        unsafe {
            let key = dlg_item(window.hwnd, ID_STATIC_REGION_KEY).unwrap();
            assert!(windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(key).as_bool());
        }
        send_command(&window, ID_STATIC_REGION_KEY_CLEAR);
        assert!(window.read(&form).cfg.anki.static_region_key.is_empty());
    }

    #[test]
    fn native_hotkey_conflict_names_both_actions_and_keys() {
        let mut config = crate::config::Config::default();
        config.trigger.trigger_key = "f2".into();
        config.actions.screenshot.hotkey = "F2".into();
        let form = crate::settings::from_config(&config, &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();

        let error = window.validate_hotkeys(&config).unwrap_err().to_string();
        assert!(error.contains("Screenshot (F2)"), "{error}");
        assert!(error.contains("Lookup trigger (f2)"), "{error}");
    }

    #[test]
    fn every_supplied_help_value_renders_once() {
        let mut layout = SettingsLayout::embedded().unwrap();
        let help = "Unique lookup key help";
        for tab in &mut layout.tabs {
            for section in &mut tab.sections {
                if let Some(entry) = section.entries.iter_mut()
                    .find(|entry| entry.id == SettingId::LookupKey)
                {
                    entry.help = Some(help.into());
                }
            }
        }
        layout.validate().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open_with_layout(
            &form,
            &[],
            ApplyMode::Standalone,
            layout,
        )
        .unwrap();
        let mut count = 0;
        // SAFETY: The loop follows live content-pane siblings.
        unsafe {
            let mut next = GetWindow(window.content, GW_CHILD);
            while let Ok(control) = next {
                if window_text(control) == help {
                    count += 1;
                }
                next = GetWindow(control, GW_HWNDNEXT);
            }
            assert!(dlg_item(window.hwnd, ID_SCREENSHOT_HINT).is_ok());
            assert!(dlg_item(window.hwnd, ID_STATIC_CAPTURE_HINT).is_ok());
        }
        assert_eq!(1, count);
    }

    #[test]
    fn embedded_row_labels_fit_the_live_label_column() {
        let layout = SettingsLayout::embedded().unwrap();
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        let row_ids = [
            SettingId::LookupKey,
            SettingId::AnkiAddKey,
            SettingId::StaticRegionKey,
            SettingId::ScreenshotKey,
            SettingId::OcrClipboardKey,
            SettingId::PopupTheme,
            SettingId::PopupFont,
            SettingId::PopupMaxWidth,
            SettingId::PopupMaxHeight,
            SettingId::PopupSummaryLength,
            SettingId::PopupLayout,
            SettingId::OcrEngine,
            SettingId::OcrLanguage,
            SettingId::OcrPasses,
            SettingId::AnkiUrl,
            SettingId::AnkiDeck,
            SettingId::AnkiModel,
            SettingId::ScreenshotTargets,
            SettingId::AnkiSelectionButtons,
            SettingId::AnkiSelectionSeparator,
            SettingId::AnkiTripleClick,
            SettingId::AnkiSentenceMode,
        ];
        for entry in layout.tabs.iter().flat_map(|tab| &tab.sections)
            .flat_map(|section| &section.entries)
            .filter(|entry| row_ids.contains(&entry.id))
        {
            assert_eq!(
                ROW_H,
                measured_text_height(window.hwnd, window.font.get(), &entry.label, LABEL_W),
                "label wrapped: {}",
                entry.label,
            );
        }
    }

    #[test]
    fn selection_combo_tables_cover_all_modes() {
        for (index, &(buttons, _)) in SELECTION_BUTTONS.iter().enumerate() {
            assert_eq!(buttons, selection_buttons_at(index as isize));
        }
        for (index, &(separator, _)) in SELECTION_SEPARATORS.iter().enumerate() {
            assert_eq!(separator, selection_separator_at(index as isize));
        }
        for (index, &(triple_click, _)) in TRIPLE_CLICKS.iter().enumerate() {
            assert_eq!(triple_click, triple_click_at(index as isize));
        }
        assert_eq!(SelectionButtons::PrimaryAdditive, selection_buttons_at(-1));
        assert_eq!(SelectionSeparator::Ellipsis, selection_separator_at(-1));
        assert_eq!(TripleClick::SenseWithExamples, triple_click_at(-1));
    }

    #[test]
    fn wm_close_records_a_quit_outcome() {
        let hwnd = HWND(4242 as *mut core::ffi::c_void);
        let _ = unsafe { wndproc(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)) };
        let got = OUTCOME.with(|c| c.get());
        assert_eq!(Some((hwnd.0 as isize, SettingsOutcome::Quit)), got);
    }

    #[test]
    fn escape_cancels_and_close_quits_while_busy() {
        let form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Live).unwrap();
        send_command(&window, 2);
        assert_eq!(Some(SettingsOutcome::Cancel), window.take_outcome());
        window.set_busy(true);
        // SAFETY: The test owns the native root window.
        unsafe {
            SendMessageW(window.hwnd, WM_CLOSE, None, None);
        }
        assert_eq!(Some(SettingsOutcome::Quit), window.take_outcome());
    }

    #[test]
    fn numeric_choices_step_the_range() {
        assert_eq!(vec![10, 15, 20], numeric_choices(10, 20, 5, 10));
    }

    // ---- field mapping ----

    fn mapping(anki_field: &str, source: &str) -> crate::config::FieldMapping {
        crate::config::FieldMapping {
            anki_field: anki_field.into(),
            source: source.into(),
        }
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

    /// The combo puts this window's `"(none)"` sentinel before `FIELD_SOURCES`.
    /// That entry shifts each read index by one. Save decodes the shifted index.
    /// A wrong index maps every field to the wrong source.
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

    /// Protects against data loss. The note type lacks `LegacyAudio`, so the
    /// window renders no row for it. The merge must keep its mapping.
    #[test]
    fn merged_field_map_keeps_a_mapping_the_model_lacks() {
        let saved = vec![
            mapping("Front", "expression"),
            mapping("LegacyAudio", "audio"),
        ];
        assert_eq!(
            vec![
                mapping("Front", "expression"),
                mapping("LegacyAudio", "audio")
            ],
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

    /// A row exists, so the user selected no source for that field.
    /// The merge must not restore the old mapping.
    #[test]
    fn merged_field_map_does_not_resurrect_a_none_row() {
        let saved = vec![mapping("Front", "expression")];
        assert!(merged_field_map(&saved, &[("Front", "(none)")]).is_empty());
    }

    /// An empty combo result has two meanings. The model can show the field,
    /// or the model can omit it. The merge must distinguish these cases.
    #[test]
    fn merged_field_map_separates_a_none_row_from_a_field_with_no_row() {
        let saved = vec![
            mapping("Front", "expression"),
            mapping("LegacyAudio", "audio"),
        ];
        assert_eq!(
            vec![mapping("LegacyAudio", "audio")],
            merged_field_map(&saved, &[("Front", "(none)")]),
        );
    }

    /// The merge keeps rendered rows in model order.
    /// It appends other mappings in their saved configuration order.
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

    /// A second Apply must not reorder the user's TOML.
    #[test]
    fn merged_field_map_is_a_fixed_point_under_a_second_apply() {
        let saved = vec![mapping("OldAudio", "audio"), mapping("Front", "sentence")];
        let readings = [("Front", "expression"), ("Back", "glossary")];
        let once = merged_field_map(&saved, &readings);
        assert_eq!(once, merged_field_map(&once, &readings));
    }

    /// AnkiConnect returned no fields, so the window rendered no row.
    /// The merge keeps the saved map.
    #[test]
    fn merged_field_map_keeps_everything_when_no_row_was_rendered() {
        let saved = vec![mapping("Front", "expression"), mapping("Back", "glossary")];
        assert_eq!(saved, merged_field_map(&saved, &[]));
    }

    /// Tests the complete field-map path. A real window renders a note type
    /// that lost a mapped field, and Apply still saves its mapping.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn reading_a_model_missing_a_mapped_field_keeps_the_mapping() {
        let saved = vec![
            mapping("Front", "expression"),
            mapping("LegacyAudio", "audio"),
        ];
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.field_map = Some(saved.clone());
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        window.populate_combos(&[], &[], vec!["Front".to_string()]);
        assert_eq!(Some(saved), window.read(&form).field_map);
    }

    fn dummy_hwnd(n: isize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    #[test]
    fn pending_capture_keys_and_screenshot_shortcut_are_validated_together() {
        let mut cfg = crate::config::Config::default();
        cfg.trigger.trigger_key = "f2".into();
        cfg.anki.static_region_key = "f3".into();
        cfg.actions.screenshot.hotkey = "f2".into();
        cfg.actions.ocr_clipboard = Some(crate::config::OcrClipboardConfig {
            hotkey: Some("f5".into()), hotkey_linux: None,
            open_sentence_search: false,
        });
        let form = crate::settings::from_config(&cfg, &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        for (id, vk) in [(ID_TRIGGER_KEY, 0x72), (ID_STATIC_REGION_KEY, 0x71),
            (ID_OCR_CLIPBOARD_KEY, 0x73)] {
            CAPTURING.with(|c| c.set(Some((window.hwnd.0 as isize, id))));
            assert!(window.handle_capture_key(vk));
        }
        CAPTURING.with(|c| c.set(Some((window.hwnd.0 as isize, ID_SCREENSHOT_HOTKEY))));
        assert!(window.handle_capture_key(0x74));
        let edited = window.read(&form);
        let pending = crate::settings::apply_to(&edited, &cfg);
        pending.validate_hotkeys(crate::config::Platform::Windows).unwrap();
        assert_eq!(pending.trigger.trigger_key, "f3");
        assert_eq!(pending.anki.static_region_key, "f2");
        assert_eq!(pending.actions.screenshot.hotkey, "f5");
        assert_eq!(pending.actions.ocr_clipboard.unwrap().hotkey.as_deref(), Some("f4"));
        assert_eq!(cfg.actions.screenshot.hotkey, "f2");

        CAPTURING.with(|c| c.set(Some((window.hwnd.0 as isize, ID_SCREENSHOT_HOTKEY))));
        assert!(window.handle_capture_key(0x72));
        let pending = crate::settings::apply_to(&window.read(&form), &cfg);
        assert!(pending.validate_hotkeys(crate::config::Platform::Windows).unwrap_err()
            .to_string().contains("Screenshot conflicts with Lookup trigger"));
        assert_eq!(window.read(&form).cfg.trigger.trigger_key, "f3");
        assert_eq!(window.read(&form).cfg.actions.screenshot.hotkey, "f3");
    }

    #[test]
    fn screenshot_key_capture_preserves_cancels_rebinds_and_clears() {
        let cfg = crate::config::Config::default();
        let form = crate::settings::from_config(&cfg, &[]);
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone).unwrap();
        assert_eq!(window.read(&form).cfg.actions.screenshot.hotkey, "ctrl+shift+s");
        assert!(!window.read(&form).screenshot_hotkey_edited);
        // SAFETY: These commands target controls owned by the live settings window.
        unsafe {
            SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(ID_SCREENSHOT_HOTKEY as usize)), None);
        }
        assert!(window.handle_capture_key(0x10));
        assert!(CAPTURING.with(|c| c.get()).is_some());
        assert!(window.handle_capture_key(0x1B));
        assert_eq!(window.read(&form).cfg.actions.screenshot.hotkey, "ctrl+shift+s");
        assert!(CAPTURING.with(|c| c.get()).is_none());
        // SAFETY: The same capture button remains live.
        unsafe {
            SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(ID_SCREENSHOT_HOTKEY as usize)), None);
        }
        assert!(window.handle_capture_key(0x78));
        assert_eq!(window.read(&form).cfg.actions.screenshot.hotkey, "f9");
        assert_eq!(crate::config::parse_trigger_key(&window.read(&form).cfg.anki.add_key),
            crate::config::parse_trigger_key(&form.cfg.anki.add_key));
        // SAFETY: The Clear button belongs to this same live window.
        unsafe {
            SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(ID_SCREENSHOT_KEY_CLEAR as usize)), None);
        }
        let cleared = window.read(&form);
        assert!(cleared.cfg.actions.screenshot.hotkey.is_empty());
        assert!(cleared.screenshot_hotkey_edited);
        assert!(crate::settings::apply_to(&cleared, &cfg).actions.screenshot.hotkey.is_empty());
        drop(window);
        assert!(SCREENSHOT_CAPTURED_VK.with(|c| c.get()).is_none());
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

    #[test]
    fn matching_prefix_result_cancels_pending_field_map() {
        let fields = vec![
            "A".to_string(),
            "B".to_string(),
            "C".to_string(),
            "D".to_string(),
        ];
        let rows = vec![
            ("A".to_string(), dummy_hwnd(1)),
            ("B".to_string(), dummy_hwnd(2)),
            ("C".to_string(), dummy_hwnd(3)),
            ("D".to_string(), dummy_hwnd(4)),
        ];
        let mut pending = Some(PendingFieldMap::new(
            vec![
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
                "E".to_string(),
            ],
            Vec::new(),
        ));
        pending.as_mut().unwrap().next = FIELD_MAP_ROWS_PER_PUMP;

        assert!(!begin_field_map_result(&fields, &rows, &mut pending));
        assert!(pending.is_none());
    }

    #[test]
    fn empty_result_cancels_pending_field_map() {
        let fields: Vec<String> = Vec::new();
        let rows = vec![("A".to_string(), dummy_hwnd(1))];
        let mut pending = Some(PendingFieldMap::new(
            vec![
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
            ],
            Vec::new(),
        ));
        pending.as_mut().unwrap().next = FIELD_MAP_ROWS_PER_PUMP;

        assert!(!begin_field_map_result(&fields, &rows, &mut pending));
        assert!(pending.is_none());
    }

    // ---- field-map columns ----

    #[test]
    fn field_map_rows_needed_ceils_by_two() {
        assert_eq!(1, field_map_rows_needed(1));
        assert_eq!(1, field_map_rows_needed(2));
        assert_eq!(2, field_map_rows_needed(3));
        assert_eq!(12, field_map_rows_needed(23));
    }

    /// The count is never zero, even for an empty list.
    #[test]
    fn field_map_rows_needed_floors_at_one() {
        assert_eq!(1, field_map_rows_needed(0));
    }

    #[test]
    fn field_map_chunk_end_caps_each_pump() {
        assert_eq!(4, field_map_chunk_end(0, 23));
        assert_eq!(8, field_map_chunk_end(4, 23));
        assert_eq!(23, field_map_chunk_end(20, 23));
    }

    #[test]
    fn column_label_keeps_a_short_name_whole() {
        assert_eq!("Glossary", column_label("Glossary"));
    }

    /// At the boundary, a name of exactly the maximum length stays whole.
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

    /// The code must cut only at a char boundary.
    #[test]
    fn column_label_is_char_boundary_safe() {
        let name = "日本語日本語日本語日本語日本語日本語日本語";
        let got = column_label(name);
        assert_eq!(18, got.chars().count());
    }

    #[test]
    fn field_map_toggle_label_shows_the_fold_direction() {
        assert_eq!("Custom mapping \u{25B6}", field_map_toggle_label("Custom mapping", true));
        assert_eq!("Custom mapping \u{25BC}", field_map_toggle_label("Custom mapping", false));
    }

    /// The client area must determine the window size. A guessed constant must
    /// not set it.
    ///
    /// `CreateWindowExW` receives the **outer** size. The 39px caption and
    /// frame can cover the Apply and Cancel buttons.
    /// `cargo test` cannot detect this size fault. The compiler cannot detect
    /// it. A desktop review measured the fault.
    ///
    /// Locks the arithmetic. The client area is smaller than the outer window
    /// that contains it. Code that treats content height as window height loses
    /// non-client overhead.
    #[test]
    fn a_client_area_is_smaller_than_its_window() {
        // We measured this value on this machine for the style this window uses.
        const CAPTION_AND_FRAME: i32 = 39;
        let content_bottom = 618 + ROW_H + 4;
        let outer_if_guessed = 620;
        assert!(
            content_bottom > outer_if_guessed - CAPTION_AND_FRAME,
            "the guessed constant must be shown to be too small, or this test proves nothing"
        );
    }

    /// The DPI scale must stay identity at 96, and grow in proportion above
    /// 96. The process is PER_MONITOR_AWARE_V2, so Windows scales nothing.
    #[test]
    fn the_dpi_scale_is_identity_at_96() {
        assert_eq!(100, (100i64 * 96 / 96) as i32);
        assert_eq!(150, (100i64 * 144 / 96) as i32);
        assert_eq!(200, (100i64 * 192 / 96) as i32);
    }

    /// The combo must offer a hand-edited value that is off the step, and
    /// must not snap it. Settings must never change a setting that the user
    /// did not touch.
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

    /// One selected file produces one path.
    #[test]
    fn a_single_pick_is_not_treated_as_a_directory() {
        assert_eq!(
            vec![PathBuf::from(r"C:\dicts\terms.zip")],
            split_picked(&nul_run(&[r"C:\dicts\terms.zip"]))
        );
    }

    /// A multi-file result gives the directory first, then each file name.
    #[test]
    fn a_multi_pick_joins_each_name_onto_the_directory() {
        assert_eq!(
            vec![
                PathBuf::from(r"C:\dicts\a.zip"),
                PathBuf::from(r"C:\dicts\b.zip")
            ],
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

    /// UTF-16 preserves non-ASCII file names.
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

    /// Windows lists a duplicate with vertical layout for each font family.
    /// The list must remove every duplicate and contain at least one family.
    #[test]
    fn the_japanese_font_list_excludes_vertical_duplicates() {
        let families = japanese_font_families();
        assert!(
            !families.is_empty(),
            "no Japanese-capable font families found"
        );
        assert!(
            !families.iter().any(|f| f.starts_with('@')),
            "got {families:?}"
        );
    }

    // ---- trigger-key capture ----

    #[test]
    fn take_captured_key_is_none_when_not_capturing() {
        let hwnd = HWND(6001 as *mut core::ffi::c_void);
        assert_eq!(None, take_captured_key(hwnd, 0x10));
    }

    /// A named key ends the capture. The call returns that name.
    #[test]
    fn take_captured_key_accepts_a_named_key() {
        let hwnd = HWND(6002 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        let got = take_captured_key(hwnd, 0x11);

        assert_eq!(Some((ID_TRIGGER_KEY, "Ctrl".to_string())), got);
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// The capture accepts a virtual key code that was not listed before.
    #[test]
    fn take_captured_key_accepts_a_previously_unlisted_key() {
        let hwnd = HWND(6003 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        let got = take_captured_key(hwnd, 0x41); // 'A'

        assert_eq!(Some((ID_TRIGGER_KEY, "A".to_string())), got);
        assert_eq!(None, CAPTURING.with(|c| c.get()), "capture must end");
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// The call records the vk, so `read()` can see it later.
    #[test]
    fn take_captured_key_records_the_vk_for_read() {
        let hwnd = HWND(6007 as *mut core::ffi::c_void);
        CAPTURING.with(|c| c.set(Some((hwnd.0 as isize, ID_TRIGGER_KEY))));

        take_captured_key(hwnd, 0x41);

        assert_eq!("0x41", resolved_trigger_key(hwnd, "shift"));
        CAPTURED_VK.with(|c| c.set(None));
    }

    /// The Anki add key uses a separate capture control.
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

    /// The Anki capture state and trigger capture state stay separate.
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

    /// The "Not set" button represents the form's `None`.
    /// An empty string must not replace it (ARCHITECTURE.md#settings-and-config).
    #[test]
    fn resolved_ocr_clipboard_key_maps_an_unset_button_to_none() {
        let hwnd = HWND(6015 as *mut core::ffi::c_void);
        OCR_CLIP_CAPTURED_VK.with(|c| c.set(None));

        assert_eq!(None, resolved_ocr_clipboard_key(hwnd, None));
        assert_eq!(None, resolved_ocr_clipboard_key(hwnd, Some("")));
        assert_eq!(
            Some("f9".to_string()),
            resolved_ocr_clipboard_key(hwnd, Some("f9"))
        );
    }

    // ---- anki add-key capture ----

    #[test]
    fn resolved_anki_add_key_falls_back_to_the_template_when_uncaptured() {
        let hwnd = HWND(6010 as *mut core::ffi::c_void);
        ANKI_CAPTURED_VK.with(|c| c.set(None));

        assert_eq!("ctrl", resolved_anki_add_key(hwnd, "ctrl"));
    }

    /// The code normalizes the default letter.
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
            assert_eq!(
                Some(vk),
                crate::config::parse_trigger_key(&stored),
                "{stored}"
            );
        }
    }

    // ---- capture size fields ----

    #[test]
    fn parse_px_reads_a_plain_number() {
        assert_eq!(640, parse_px("640", 500));
    }

    /// User input can contain spaces around the number.
    #[test]
    fn parse_px_ignores_surrounding_space() {
        assert_eq!(640, parse_px("  640 ", 500));
    }

    /// Invalid input must keep the fallback value.
    #[test]
    fn parse_px_keeps_the_old_value_for_junk() {
        assert_eq!(500, parse_px("", 500));
        assert_eq!(500, parse_px("abc", 500));
        assert_eq!(500, parse_px("640px", 500));
        assert_eq!(500, parse_px("6.4", 500));
    }

    // ---- apply caption ----

    /// Live mode applies immediately when no changes are staged.
    #[test]
    fn a_live_window_with_nothing_staged_just_applies() {
        assert_eq!("Apply", apply_caption(ApplyMode::Live));
        assert!(apply_hint(ApplyMode::Live, false).contains("right away"));
    }

    /// A staged update needs no rebuild or restart.
    #[test]
    fn a_staged_dictionary_promises_an_in_place_update() {
        assert_eq!("Apply", apply_caption(ApplyMode::Live));
        let hint = apply_hint(ApplyMode::Live, true);
        assert!(hint.contains("in place"), "{hint}");
        assert!(!hint.contains("rebuild"), "{hint}");
        assert!(!hint.contains("restart"), "{hint}");
    }

    /// Standalone mode restarts chibipop after Apply.
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

    /// A configured language remains in the list when it is not installed.
    #[test]
    fn a_configured_language_missing_from_the_list_is_appended() {
        let got = language_choices(installed(), "ko");
        assert_eq!(3, got.len());
        assert_eq!(("ko (not installed)".to_string(), "ko".to_string()), got[2]);
    }

    /// An empty installed-language list still shows the configured language.
    #[test]
    fn an_empty_list_still_offers_the_configured_language() {
        assert_eq!(
            vec![("ja (not installed)".to_string(), "ja".to_string())],
            language_choices(Vec::new(), "ja")
        );
    }

    /// The row keeps the display name and the tag.
    #[test]
    fn an_installed_language_keeps_its_display_name_and_its_tag() {
        let got = language_choices(installed(), "ja");
        assert_eq!(installed(), got);
        assert_eq!("Japanese", got[0].0);
        assert_eq!("ja", got[0].1);
    }

    #[test]
    fn the_installed_order_is_the_listed_order() {
        let tags: Vec<String> = language_choices(installed(), "ja")
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(vec!["ja".to_string(), "en-US".to_string()], tags);
    }

    /// A case mismatch must not produce an empty combo.
    #[test]
    fn a_configured_tag_matches_its_entry_whatever_its_case() {
        let rows = language_choices(installed(), "EN-us");
        assert_eq!(2, rows.len());
        assert_eq!(Some(1), language_index(&rows, "EN-us"));
    }

    /// An empty configured language must not create a blank row.
    #[test]
    fn an_empty_configured_language_is_not_offered_as_a_blank_row() {
        assert!(language_choices(Vec::new(), "").is_empty());
        assert_eq!(installed(), language_choices(installed(), ""));
    }

    /// `read` returns this tag.
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
            (
                "Chinese (Traditional)".to_string(),
                "zh-Hant-TW".to_string(),
            ),
        ]
    }

    #[test]
    fn a_configured_prefix_is_not_appended_as_a_phantom_row() {
        assert_eq!(
            installed_four(),
            language_choices(installed_four(), "zh-Hans")
        );
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

    /// FIX 1 behavior remains active.
    #[test]
    fn a_genuinely_absent_language_is_still_appended_and_read_back() {
        let rows = language_choices(installed_four(), "ko");
        assert_eq!(5, rows.len());
        assert_eq!(
            ("ko (not installed)".to_string(), "ko".to_string()),
            rows[4]
        );
        assert_eq!(Some(4), language_index(&rows, "ko"));
    }

    /// The match uses a subtag boundary, not `starts_with`.
    #[test]
    fn a_partial_subtag_is_treated_as_absent() {
        let rows = language_choices(installed_four(), "zh-Han");
        assert_eq!(5, rows.len());
        assert_eq!(Some(4), language_index(&rows, "zh-Han"));
        assert_eq!("zh-Han", rows[4].1);
    }

    /// The first installed language that matches is selected. The choice is arbitrary.
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

    /// Shared identifiers make `dlg_item` return the wrong control.
    /// One list can then answer for two sections.
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

    /// Each Move button names one section. It must not act on the list
    /// selected last. Three independent lists remove this ambiguity.
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

    /// Each section has an Add button. All three buttons have the same action.
    /// The archive's roles select the destination lists.
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

    /// comctl32 stores checkbox state in the nibble that
    /// `LVIS_STATEIMAGEMASK` covers. Index 1 means clear, and index 2 means ticked.
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

    /// Selection and focus share the checkbox state word. A selected clear row
    /// must not read as ticked. Otherwise a click can enable every row.
    #[test]
    fn selection_and_focus_bits_do_not_read_as_a_tick() {
        let live = LVIS_SELECTED.0 | LVIS_FOCUSED.0;
        assert!(!state_is_checked(live));
        assert!(!state_is_checked(check_state(false) | live));
        assert!(state_is_checked(check_state(true) | live));
    }

    /// A row that predates the extended style has no state image. A row
    /// with no box carries no tick.
    #[test]
    fn a_row_with_no_state_image_reads_as_clear() {
        assert!(!state_is_checked(0));
    }

    // ---- Move rows inside one section ----

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

    /// A selection index beyond the last row cannot reorder the list.
    #[test]
    fn a_move_from_beyond_the_last_row_refuses() {
        assert_eq!(None, move_target(2, 2, true));
        assert_eq!(None, move_target(2, 5, false));
        assert_eq!(None, move_target(0, 0, true));
    }

    /// A one-row list has no second row for a move. An empty enabled list means
    /// "search nothing" (ARCHITECTURE.md#dictionary-and-lookup).
    /// The only row in the section cannot move.
    #[test]
    fn the_only_row_in_a_section_can_move_neither_way() {
        assert_eq!(None, move_target(1, 0, true));
        assert_eq!(None, move_target(1, 0, false));
    }

    /// A disabled button reports the move condition but does not move a row.
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

    /// A control that is not there reports a negative count.
    #[test]
    fn an_absent_list_greys_both_move_buttons() {
        assert!(!can_move(-1, 0, true));
        assert!(!can_move(-1, 0, false));
    }

    // ---- Drag a row into place ----

    /// A small pointer move on a checkbox must remain a click.
    /// The pointer must exceed the deadband before the list starts a drag.
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

    /// Movement on either axis starts a drag in either direction.
    #[test]
    fn travel_of_the_floor_on_one_axis_becomes_a_drag() {
        assert!(clears_drag_deadband((40, 30), (40, 30 + DRAG_DEADBAND_PX)));
        assert!(clears_drag_deadband((40, 30), (40, 30 - DRAG_DEADBAND_PX)));
        assert!(clears_drag_deadband((40, 30), (40 + DRAG_DEADBAND_PX, 30)));
        assert!(clears_drag_deadband((40, 30), (40 - DRAG_DEADBAND_PX, 30)));
    }

    /// A row is 17px tall (see `DICT_LIST_H`). A three-row list has boundaries
    /// at 0, 17, 34, and 51. Each row uses the nearer boundary. The list draws
    /// the insertion mark on that boundary.
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

    /// The function clamps a cursor to this list. A cursor outside this list
    /// or over another list uses the first or last gap here. A row cannot enter
    /// a list that has no such role (ARCHITECTURE.md#dictionary-and-lookup).
    #[test]
    fn a_cursor_outside_the_list_clamps_to_that_lists_own_ends() {
        assert_eq!(0, drop_gap(-9, 0, 17, 3), "a row and a half above it");
        assert_eq!(0, drop_gap(-4000, 0, 17, 3), "far above the window");
        assert_eq!(3, drop_gap(4000, 0, 17, 3), "far below the window");
    }

    /// Row 0 starts at the scroll offset. A scrolled list needs no second
    /// offset. Every gap moves with the list, so the cursor still identifies
    /// the visible row.
    #[test]
    fn a_scrolled_list_reads_its_gaps_from_row_zeros_own_top() {
        assert_eq!(2, drop_gap(0, -34, 17, 6));
        assert_eq!(3, drop_gap(17, -34, 17, 6));
    }

    /// A control with no rows has no gap for a drop. A control that reports
    /// no row height must not divide by that height.
    #[test]
    fn a_list_with_no_rows_or_no_height_reads_the_first_gap() {
        assert_eq!(0, drop_gap(80, 0, 17, 0));
        assert_eq!(0, drop_gap(80, 0, 0, 3));
    }

    /// The dragged row leaves its old position. The gap below it maps to that
    /// position, and each later gap maps to the row before it.
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

    /// The mark identifies a row and its side. Only the gap after the last row
    /// lies beyond a row.
    #[test]
    fn the_insertion_mark_sits_above_a_gaps_row_except_past_the_last() {
        assert_eq!((0, 0), insert_mark_at(0, 3));
        assert_eq!((1, 0), insert_mark_at(1, 3));
        assert_eq!((2, 0), insert_mark_at(2, 3));
        assert_eq!((2, LVIM_AFTER), insert_mark_at(3, 3));
    }

    /// States the acceptance criterion as arithmetic. A cursor below the list
    /// places the row last. A cursor above it places the row first.
    /// Both results refer to the source list.
    #[test]
    fn a_drag_off_either_end_lands_the_row_at_that_end_of_its_own_list() {
        assert_eq!(2, drop_target(0, drop_gap(4000, 0, 17, 3)));
        assert_eq!(0, drop_target(2, drop_gap(-4000, 0, 17, 3)));
    }

    /// One rule defines a move. A drop must match each Move-button step that
    /// the row crosses. The result must equal a list that removes the row and
    /// inserts it at the drop position. The test checks every row and gap.
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

    /// Returns the center of one row in screen coordinates.
    ///
    /// The test moves the real cursor because `track_drag` and `finish_drag`
    /// read it. A captured drag uses the frame of the window that owns capture.
    /// `drop_gap` needs coordinates in the list's frame.
    unsafe fn row_centre(list: HWND, index: i32) -> POINT {
        // SAFETY: the caller owns `list`, a live ListView. `rect` and `pt`
        // are writable stack storage that outlives every call.
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

    /// Sends the ListView notification that starts a drag.
    /// The cursor position becomes the action point.
    unsafe fn send_begin_drag(hwnd: HWND, list: HWND, id: i32, item: i32) {
        // SAFETY: the caller owns `hwnd` and `list`, both live windows. `nm`
        // is fully initialized stack storage that outlives the send, which
        // is the contract that WM_NOTIFY's `lparam` carries.
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

    /// Moves the cursor and sends a mouse-move message to the window.
    /// The window receives the message while it owns capture.
    unsafe fn drag_cursor_to(hwnd: HWND, pt: POINT) {
        // SAFETY: the caller owns `hwnd`, a live window. Neither message
        // carries a pointer.
        unsafe {
            let _ = SetCursorPos(pt.x, pt.y);
            SendMessageW(hwnd, WM_MOUSEMOVE, None, None);
        }
    }

    /// Every section gets three rows, so a drag has a target in each one.
    fn three_of_each() -> SettingsForm {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Terms A", true), ("Terms B", true), ("Terms C", true)]);
        form.frequency = rows(&[("Freq A", true), ("Freq B", true), ("Freq C", true)]);
        form.pitch = rows(&[("Pitch A", true), ("Pitch B", true), ("Pitch C", true)]);
        form
    }

    /// Drives the full gesture on real controls. The ListView notification
    /// starts the gesture. The cursor sets the destination. Button release
    /// commits the same path as the Move buttons, so selection follows the row.
    ///
    /// The test does not check the insertion mark. Wine's comctl32 returns 0
    /// for both `LVM_SETINSERTMARK` and `LVM_GETINSERTMARK`, so it draws no mark.
    /// `drop_gap` and `insert_mark_at` set the mark position. Other tests check
    /// them without controls.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_row_dragged_onto_the_first_row_becomes_the_first_row() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: `h` is the window that this test opened, and it stays live
        // for the whole test. `ID_TERMS` names the list that `build` created
        // inside it, and every helper here states its own contract.
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

    /// Each role has its own order, so a drag cannot enter another role list.
    /// A release over another section places the row at the end of its source
    /// list. The other section remains unchanged
    /// (ARCHITECTURE.md#dictionary-and-lookup).
    /// The test checks movement toward both ends because sections stack vertically.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drag_over_another_roles_list_clamps_to_its_own_end_and_leaves_that_list_alone() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: as above. All three ids name lists that `build` created.
        let (terms, freqs, pitch) = unsafe {
            let terms = dlg_item(h, ID_TERMS).expect("the terms list");
            let freqs = dlg_item(h, ID_FREQS).expect("the frequency list");
            let pitch = dlg_item(h, ID_PITCH).expect("the pitch list");
            // Down and out of Terms, then release on a Frequency row.
            drag_cursor_to(h, row_centre(terms, 0));
            send_begin_drag(h, terms, ID_TERMS, 0);
            drag_cursor_to(h, row_centre(freqs, 1));
            SendMessageW(h, WM_LBUTTONUP, None, None);
            // Up and out of Pitch, then release on a Terms row.
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

    /// A row has a checkbox. A pointer action can change its state or start a drag.
    /// The test checks the state change and the unchanged row order.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_click_on_a_rows_checkbox_ticks_it_and_moves_nothing() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: As above. `lv_check` states its own contract.
        let order = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            let on_the_box = row_centre(list, 0);
            lv_check(list, 0, false);
            drag_cursor_to(h, on_the_box);
            // The control treated the action as a drag. The pointer moved two
            // pixels, which does not reorder the list.
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

    /// A release outside the window cancels the drag. The row stays in place,
    /// mouse capture returns, and no drag state remains.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drag_released_outside_the_window_changes_nothing_and_gives_the_mouse_back() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: As above. `GetWindowRect`, `GetCursorPos`, and `PtInRect`
        // all write into or read from stack storage that outlives them.
        let (off_window, order, captured, dragging) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            drag_cursor_to(h, row_centre(list, 0));
            send_begin_drag(h, list, ID_TERMS, 0);
            let mut rect = RECT::default();
            let _ = GetWindowRect(h, &mut rect);
            // Place the cursor beside the window rather than below it. The window
            // is taller than it is wide, so the side provides space.
            let middle = (rect.top + rect.bottom) / 2;
            let beside = if rect.left > 40 { rect.left - 40 } else { rect.right + 40 };
            drag_cursor_to(h, POINT { x: beside, y: middle });
            // Read the cursor position to confirm that the desktop clipped it
            // to its bounds.
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

    /// Another component can take capture during a drag.
    /// That action ends the gesture. The next button-up must not move the row.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_stolen_capture_ends_the_drag_and_leaves_no_row_in_the_air() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: As above. A live control in this window takes and returns
        // capture.
        let (dragging, order) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            drag_cursor_to(h, row_centre(list, 0));
            send_begin_drag(h, list, ID_TERMS, 0);
            // Move far enough that a drop would change the order.
            drag_cursor_to(h, row_centre(list, 2));
            SetCapture(list);
            let dragging = drag_of(h).is_some();
            let _ = ReleaseCapture();
            // The button-up event would otherwise commit the drop.
            SendMessageW(h, WM_LBUTTONUP, None, None);
            (dragging, lv_rows(h, ID_TERMS))
        };

        assert!(!dragging, "the steal has to end the gesture");
        assert_eq!(Some(form.terms.clone()), order, "and no drop may follow it");
    }

    /// Tests the shared path after a drop at the top.
    /// The Move up button becomes disabled. Focus must leave it first because
    /// Windows keeps focus on a disabled control.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn a_drop_at_the_top_greys_move_up_and_takes_the_focus_off_it() {
        let form = three_of_each();
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");
        let h = window.hwnd();

        // SAFETY: As above. `IsWindowEnabled`, `GetFocus`, and `SetFocus`
        // read and move focus between live controls in this window.
        let (parked, order, live, focused) = unsafe {
            let list = dlg_item(h, ID_TERMS).expect("the terms list");
            let up = dlg_item(h, ID_TERMS_UP).expect("the terms Move up button");
            // Row 1 can move up, so the button is enabled and can receive focus.
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

    /// The table defines both combo labels and strategies. Every label that
    /// `build` adds must read back as the strategy at the same index.
    #[test]
    fn the_ranking_combo_reads_back_the_strategy_at_each_index() {
        for (at, (strategy, _)) in RANKING_STRATEGIES.iter().enumerate() {
            assert_eq!(*strategy, ranking_strategy_at(at as isize));
        }
    }

    /// `build` selects item 0 when no configured value matches.
    /// A lost selection must read as item 0.
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

    // ---- Rescope the Terms list ----

    fn installed_two() -> Vec<String> {
        vec![
            "Jitendex.org [2026-07-09]".to_string(),
            "大辞林　第四版".to_string(),
        ]
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

    /// An empty list leaves every row ticked.
    #[test]
    fn an_empty_language_list_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &[], &[])
        );
    }

    /// The list matches exact names, not prefixes.
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

    /// A stale list leaves every row ticked. A partial name is also stale.
    /// The rule rejects substring matches for renamed dictionaries.
    #[test]
    fn a_list_matching_nothing_installed_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &names(&["大辞林"]), &[])
        );
    }

    /// Blank entries do not select any dictionary.
    #[test]
    fn a_blank_only_list_leaves_every_row_ticked() {
        assert_eq!(
            rows(&[("Jitendex.org [2026-07-09]", true), ("大辞林　第四版", true)]),
            scope_rows(&installed_two(), &[String::new()], &[])
        );
    }

    /// An unreadable row cannot select a scope.
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

    /// The unreadable row must remain removable. Terms is the only section
    /// that lists a dictionary with no roles
    /// (ARCHITECTURE.md#dictionary-and-lookup).
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

    /// The list sits beside four buttons, so its height cannot be less than
    /// one row.
    #[test]
    fn a_role_list_is_as_tall_as_its_four_button_column() {
        assert_eq!(3 * BTN_PITCH + ROW_H, DICT_LIST_H);
    }

    /// Only Frequency has a ranking rule. Only its group includes the ranking row.
    #[test]
    fn only_the_frequency_group_is_taller_by_the_strategy_row() {
        let plain = 20 + DICT_CAP_H + DICT_LIST_H + 8;
        assert_eq!(plain, role_group_h(Role::Terms));
        assert_eq!(plain, role_group_h(Role::Pitch));
        assert_eq!(plain + ROW_H + ROW_GAP, role_group_h(Role::Frequency));
    }

    /// Tests the complete path. A real window renders three role sections.
    /// Read-back must preserve each row's checkbox and position.
    /// The strategy combo must also round-trip.
    ///
    /// Needs a real desktop session.
    #[test]
    #[ignore]
    fn three_role_sections_read_back_every_row_with_its_checkbox() {
        let mut form = crate::settings::from_config(&crate::config::Config::default(), &[]);
        form.terms = rows(&[("Terms A", true), ("Terms B", false)]);
        form.frequency = rows(&[("Freq A", true), ("Freq B", true)]);
        form.pitch = rows(&[("Pitch A", false)]);
        form.cfg.dictionaries.ranking_strategy = RankingStrategy::Median;
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");

        let back = window.read(&form);

        assert_eq!(form.terms, back.terms);
        assert_eq!(form.frequency, back.frequency);
        assert_eq!(form.pitch, back.pitch);
        assert_eq!(RankingStrategy::Median, back.cfg.dictionaries.ranking_strategy);
    }

    /// A checkbox affects only its section. A Move button affects only its
    /// adjacent list. A dictionary with two roles has two rows. If the user
    /// clears its definitions, its frequency data must not change
    /// (ARCHITECTURE.md#dictionary-and-lookup).
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

        // SAFETY: `h` is the window that this test opened, and it stays live
        // for the whole test. Both ids name controls that `build` created.
        // Each `lv_*` helper states its own contract.
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

    /// Every row remains visible and removable, even when it has no roles.
    /// Terms carries unreadable archives for this reason, so Remove must be
    /// enabled when the user selects the row.
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

        // SAFETY: As above. `IsWindowEnabled` reads a live control.
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

    /// The window passes values to the core path. A frequency reorder, tick, or
    /// strategy change requests a reindex. A Terms or Pitch change writes config
    /// and uses the stored `reload`.
    /// `settings::dictionary_work` owns this rule. The test checks that each
    /// control reaches it. A `read` step that omits the strategy skips a rank update.
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
        // Create one window for each change. Otherwise changes could accumulate.
        let work = |touch: &dyn Fn(HWND)| {
            let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
                .expect("opening the settings window");
            touch(window.hwnd());
            let after = crate::settings::apply_to(&window.read(&form), &before);
            crate::settings::dictionary_work(&before, &after)
        };
        let reorder = |list: i32, down: i32| {
            move |h: HWND| {
                // SAFETY: Both ids name controls that `build` created inside `h`.
                // `lv_select` states its own contract. The code sends the same
                // message as a click.
                unsafe {
                    let l = dlg_item(h, list).expect("a role list");
                    lv_select(l, 0);
                    SendMessageW(h, WM_COMMAND, Some(WPARAM(down as usize)), None);
                }
            }
        };
        let untick = |list: i32| {
            move |h: HWND| {
                // SAFETY: As above. `lv_check` states its own contract.
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
                // SAFETY: `ID_RANKING` names the combo that `build` created
                // inside `h`. It stays live for this call.
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

    // ---- Plugins ----

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
        let m = manifest_stub(
            "meikiocr",
            vec![crate::plugin::manifest::Role::TextProvider],
        );
        let row = plugin_row(Path::new("meikiocr"), &Ok(m), &["meikiocr".to_string()]);
        assert_eq!("Enabled", row.status);
        assert!(row.checked);
        assert!(row.can_enable);
        assert_eq!("meikiocr 0.1.0", row.label);
    }

    #[test]
    fn an_unlisted_plugin_is_labelled_disabled() {
        let m = manifest_stub(
            "meikiocr",
            vec![crate::plugin::manifest::Role::TextProvider],
        );
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
        let m = manifest_stub(
            "meikiocr",
            vec![crate::plugin::manifest::Role::TextProvider],
        );
        let found = vec![(PathBuf::from("meikiocr"), Ok(m))];
        let names = discovered_text_providers(&found);
        assert_eq!(vec!["meikiocr".to_string()], names);
    }

    #[test]
    fn discovered_text_providers_excludes_a_non_provider_role() {
        let m = manifest_stub(
            "scorer",
            vec![crate::plugin::manifest::Role::FieldContributor],
        );
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
        assert_eq!(
            "text-provider",
            roles_text(&[crate::plugin::manifest::Role::TextProvider])
        );
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
        let m = manifest_stub(
            "meikiocr",
            vec![crate::plugin::manifest::Role::TextProvider],
        );
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
        assert_eq!(
            None,
            plugin_configure_idx(ID_PLUGIN_CONFIGURE_BASE + PLUGIN_ID_SPAN)
        );
        assert_eq!(None, plugin_configure_idx(ID_PLUGIN_ENABLE_BASE));
    }

    #[test]
    fn plugin_dir_at_reads_back_what_build_remembered() {
        let hwnd = dummy_hwnd(9101);
        remember_plugin_dirs(hwnd, vec![PathBuf::from("plugins/meikiocr")]);
        assert_eq!(
            Some(PathBuf::from("plugins/meikiocr")),
            plugin_dir_at(hwnd, 0)
        );
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
        assert_eq!(
            dirs.get("meikiocr").unwrap().as_os_str(),
            "plugins/meikiocr"
        );
        assert!(!dirs.contains_key("nonexistent"));
    }

    #[test]
    fn write_config_replaces_existing_path() {
        let existing = "meikiocr_path = \"\"\nhf_home = ''\nthreads = 4\n";
        let result = set_config_path(existing, r"C:\tools\meikiocr\.venv\Lib\site-packages");
        assert!(
            result.contains(r#"meikiocr_path = "C:\\tools\\meikiocr\\.venv\\Lib\\site-packages""#)
        );
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
