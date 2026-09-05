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
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
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
/// The Engine combo box on the OCR tab.
const ID_ENGINE: i32 = 146;
/// The Configure button on the OCR tab.
const ID_ENGINE_CONFIGURE: i32 = 147;
/// The Engine log checkbox on the OCR tab.
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
/// The Sentence combo box on the Anki tab.
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
/// The Layout mode combo box on the General tab.
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
        form.screenshot_fixed_region,
        form.screenshot_fixed_window.as_ref(),
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
const PAD: i32 = 14;
const ROW_H: i32 = 24;
const ROW_GAP: i32 = 6;
const GROUP_GAP: i32 = 10;
const BTN_W: i32 = 120;
const BTN_PITCH: i32 = ROW_H + 4;
const LABEL_W: i32 = 178;
const FIELD_X: i32 = PAD + LABEL_W;
const FIELD_W: i32 = WIN_W - FIELD_X - PAD - 16;
/// The status control has space for about three lines of text.
const STATUS_H: i32 = 58;
/// The first vertical coordinate below the tab strip.
const CONTENT_Y: i32 = PAD + TAB_H + 4;
/// The vertical offset below the top of the bottom row.
const BOTTOM_UPDATE_DY: i32 = 20;
const BOTTOM_STATUS_DY: i32 = BOTTOM_UPDATE_DY + ROW_H + 8 + GROUP_GAP;
const BOTTOM_BTN_DY: i32 = BOTTOM_STATUS_DY + STATUS_H + 2;
/// The height of the bottom row.
const BOTTOM_H: i32 = BOTTOM_BTN_DY + ROW_H + 8;
/// The horizontal coordinate of the right-aligned Apply button.
const BOTTOM_APPLY_X: i32 = WIN_W - PAD - 144;
/// Each bottom-row item stores a control identifier, a horizontal coordinate,
/// and a vertical offset.
const BOTTOM_ROW: [(i32, i32, i32); 5] = [
    (ID_UPDATES, PAD - 6, 0),
    (ID_CHECK_UPDATE, PAD, BOTTOM_UPDATE_DY),
    (ID_STATUS, PAD, BOTTOM_STATUS_DY),
    (ID_APPLY, BOTTOM_APPLY_X, BOTTOM_BTN_DY),
    (ID_QUIT, PAD, BOTTOM_BTN_DY),
];
/// The height of one scroll line at 96 DPI.
const SCROLL_LINE: i32 = 20;
/// The number of scroll lines per wheel notch.
const WHEEL_LINES: i32 = 3;

// ---- Dictionaries tab ----

/// The height of one text line above each list.
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

// ---- Plugins tab ----

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
    /// The caption of the group box.
    group: &'static str,
    /// The text line above the list.
    hint: &'static str,
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
    // SAFETY: The FFI call accepts a window handle. An invalid handle returns
    // 0, and the caller then uses 96 DPI. The fallback leaves the size unchanged.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (v as i64 * dpi as i64 / 96) as i32
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
    let top = ch - dpi_scale(hwnd, BOTTOM_H + PAD);
    // SAFETY: Each identifier in `BOTTOM_ROW` names a direct child that
    // `hwnd` created in `build`. `GetDlgItem` returns `Err` on failure.
    // `panes` has the same contract. `SWP_NOSIZE` keeps control sizes,
    // `SWP_NOMOVE` keeps the band origin, and `SWP_NOZORDER` keeps
    // the z-order position from `place_viewport`.
    unsafe {
        for (id, x, dy) in BOTTOM_ROW {
            let Ok(c) = GetDlgItem(Some(hwnd), id) else {
                continue;
            };
            let _ = SetWindowPos(
                c,
                None,
                dpi_scale(hwnd, x),
                top + dpi_scale(hwnd, dy),
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let Ok((viewport, _)) = panes(hwnd) else {
            return;
        };
        let band = (top - dpi_scale(hwnd, CONTENT_Y)).max(0);
        let _ = SetWindowPos(
            viewport,
            None,
            0,
            0,
            dpi_scale(hwnd, WIN_W),
            band,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
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
        fMask: SIF_RANGE,
        ..Default::default()
    };
    // SAFETY: `si` starts with its own size and the call receives a mutable
    // pointer. `set_scroll_range` stores the height as `nMax + 1`, so this
    // read returns the exact height.
    if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut si) }.is_err() {
        return;
    }
    set_scroll_range(hwnd, si.nMax + 1, client_h(viewport));
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
/// height, so short tabs need no scrollbar. `view_h` comes from the viewport.
/// The position resets to 0 because pane content changed.
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
    // SAFETY: `hwnd` is the settings window. `si` is initialized and passed
    // as a const pointer. `SetScrollInfo` reads `si` during the call.
    unsafe { SetScrollInfo(hwnd, SB_VERT, &si, true) };
    move_content(hwnd, 0);
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

    // Stores the field-map toggle click flag for each `HWND`.
    static FIELD_MAP_TOGGLE: Cell<Option<isize>> = const { Cell::new(None) };

    // Stores a queued Anki model selection.
    static ANKI_MODEL_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    // Stores a queued OCR language selection.
    static LANG_CHANGED: Cell<Option<isize>> = const { Cell::new(None) };

    // Stores plugin directories for each `HWND`.
    static PLUGIN_DIRS: RefCell<Option<(isize, Vec<PathBuf>)>> = const { RefCell::new(None) };

    // Stores active drag-row state for each `HWND`.
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
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
        let _ = SetWindowTextW(btn, w!("Press a key..."));
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
                ID_ANKI_ADD_KEY => unsafe { begin_capture(hwnd, ID_ANKI_ADD_KEY) },
                ID_STATIC_REGION_KEY => unsafe { begin_capture(hwnd, ID_STATIC_REGION_KEY) },
                ID_OCR_CLIPBOARD_KEY => unsafe { begin_capture(hwnd, ID_OCR_CLIPBOARD_KEY) },
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
            // The position clamp also routes to this branch.
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
            // The high word contains the signed delta. The low word contains key state.
            let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
            let step = delta / WHEEL_DELTA as i32 * WHEEL_LINES * dpi_scale(hwnd, SCROLL_LINE);
            // Wheel rotation moves the content toward the top.
            scroll_to(hwnd, |si| si.nPos - step);
            LRESULT(0)
        }
        WM_CLOSE => {
            // The message produces the same outcome as the Escape key.
            record_outcome(hwnd, SettingsOutcome::Cancel);
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
            return Err(Error::from_thread()).context("RegisterClassExW");
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
            return Err(Error::from_thread()).context("RegisterClassExW for the pane class");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Gets the system user interface font.
///
/// Returns `None` to keep the default font.
unsafe fn ui_font() -> Option<HFONT> {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `ncm` is stack storage with a size that matches its `cbSize`
    // field, as the SystemParametersInfoW contract requires.
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
    // SAFETY: `SystemParametersInfoW` populated `lfMessageFont` above.
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

/// Shows or hides controls for static capture mode.
unsafe fn update_static_controls(hwnd: HWND) {
    // SAFETY: Each identifier names a valid descendant of `hwnd`
    // created in `build`.
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
fn field_map_toggle_label(collapsed: bool) -> &'static str {
    if collapsed {
        "Field mapping \u{25B6}"
    } else {
        "Field mapping \u{25BC}"
    }
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
        .map_or_else(|| template.to_string(), stored_trigger_key)
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
pub struct SettingsWindow {
    hwnd: HWND,
    /// Viewport window that clips the content pane.
    viewport: HWND,
    /// Content window that scrolls inside the viewport pane.
    content: HWND,
    font: Option<HFONT>,
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
    /// Control handles for the General tab only.
    general_ctrls: Vec<HWND>,
    /// Control handles for the Dictionaries tab.
    dict_ctrls: Vec<HWND>,
    /// Control handles for the OCR and Debug tab.
    ocr_ctrls: Vec<HWND>,
    /// Control handles for the Anki tab only.
    anki_ctrls: Vec<HWND>,
    /// Control handles for the Plugins tab only.
    plugin_ctrls: Vec<HWND>,
    /// Plugin names in checkbox order.
    plugin_names: Vec<String>,
    /// Map from Anki field name to combo box handle.
    field_map_rows: RefCell<Vec<(String, HWND)>>,
    /// Handles for field-map labels and group box.
    field_map_extra: RefCell<Vec<HWND>>,
    pending_field_map: RefCell<Option<PendingFieldMap>>,
    /// True when the field-map section is collapsed.
    field_map_collapsed: Cell<bool>,
    /// Vertical coordinate where static Anki rows end.
    anki_static_bottom: i32,
    /// Height of each tab page in 96-DPI pixels.
    tab_heights: [i32; 5],
    /// Maximum bottom vertical coordinate among all tabs.
    bottom_y0: i32,
    /// Index of the active tab.
    current_tab: Cell<u32>,
    /// Operation mode for the Apply button.
    apply_mode: ApplyMode,
    /// True while the settings controls are disabled.
    busy: Cell<bool>,
}

impl SettingsWindow {
    /// Creates and displays a settings window from `form`.
    ///
    /// `stale` lists configured dictionary names that are not installed.
    /// The window displays a warning dialog when this list is not empty.
    /// The dialog names those dictionaries.
    ///
    /// `mode` sets the Apply button label and action.
    pub fn open(form: &SettingsForm, stale: &[String], mode: ApplyMode) -> Result<SettingsWindow> {
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
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VSCROLL,
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

            let font = ui_font();
            let mut win = SettingsWindow {
                hwnd,
                // `build` creates the viewport and content panes.
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
                pending_field_map: RefCell::new(None),
                field_map_collapsed: Cell::new(true),
                anki_static_bottom: 0,
                tab_heights: [0; 5],
                bottom_y0: 0,
                current_tab: Cell::new(0),
                apply_mode: mode,
                busy: Cell::new(false),
            };
            // `build` reports final layout height. The window sizes to
            // match content dimensions. Window frame borders and title bar
            // are accounted for so buttons remain visible across display DPIs.
            let content_h = win.build(form, stale)?;
            // Populates both sides from a single vector.
            if let Some(tag) = win.selected_language() {
                win.staged.borrow_mut().dict_list_language = tag;
            }
            // Adjusts size and shows the window. Refer to `fit_to` for why
            // `ShowWindow` is not used here.
            win.fit_to(WIN_W, content_h + PAD);
            // Displays the General tab at top scroll offset.
            win.reset_scroll();
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
        self.pump_field_map();
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
            staged.screenshot_fixed_region = None;
            staged.screenshot_fixed_window = None;
            staged.screenshot_reset_targets = true;
        }
        let summary = wide("No saved screenshot targets.");
        // SAFETY: These controls are created by `build` and remain live until
        // this window drops.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_SCREENSHOT_SUMMARY) {
                let _ = SetWindowTextW(c, PCWSTR(summary.as_ptr()));
            }
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
            staged.screenshot_fixed_region = screenshot.fixed_region;
            staged.screenshot_fixed_window = screenshot.fixed_window.clone();
            (
                staged.screenshot_fixed_region,
                staged.screenshot_fixed_window.clone(),
            )
        };
        let summary = wide(&screenshot_target_summary_values(region, window.as_ref()));
        let has_target = !self.busy.get() && (region.is_some() || window.is_some());
        // SAFETY: These controls are created by `build` and remain live until
        // this window drops. `SetWindowTextW` copies the string during the call.
        unsafe {
            if let Ok(c) = dlg_item(self.hwnd, ID_SCREENSHOT_SUMMARY) {
                let _ = SetWindowTextW(c, PCWSTR(summary.as_ptr()));
            }
            if let Ok(c) = dlg_item(self.hwnd, ID_SCREENSHOT_RESET) {
                let _ = EnableWindow(c, has_target);
            }
        }
    }

    /// Updates controls to show applied dimensions.
    pub fn set_capture_fields(&self, ocr: &crate::config::OcrConfig) {
        // SAFETY: `ID_CAPTURE_W` and `ID_CAPTURE_H` are valid descendants of
        // `self.hwnd` created in `build`. Each `dlg_item` lookup is validated,
        // and `SetWindowTextW` copies text buffers during execution.
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
    }

    /// Updates Apply button label and status text.
    fn refresh_apply(&self) {
        let staged = self.staged.borrow();
        let has_staged = staged.has_staged();
        // SAFETY: `ID_APPLY` and `ID_STATUS` are valid child windows of
        // `self.hwnd` created in `build`. `SetWindowTextW` copies the strings.
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
                            && (staged.screenshot_fixed_region.is_some()
                                || staged.screenshot_fixed_window.is_some())
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

    /// Tab page height in page coordinates.
    ///
    /// The Anki tab expands with field-map rows. The method measures
    /// this height from the field-map rows.
    fn tab_page_h(&self, tab: u32) -> i32 {
        if tab == 3 {
            return self.field_map_bottom() - CONTENT_Y;
        }
        self.tab_heights.get(tab as usize).copied().unwrap_or(0)
    }

    /// Recalculates scroll range for the active tab and scrolls to top.
    fn reset_scroll(&self) {
        let content_h = dpi_scale(self.hwnd, self.tab_page_h(self.current_tab.get()));
        set_scroll_range(self.hwnd, content_h, client_h(self.viewport));
    }

    /// Shows the selected tab and hides all other tabs.
    pub fn switch_tab(&self, tab: u32) {
        // SAFETY: `self.hwnd` remains valid until `Drop`.
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
        // SAFETY: Every window handle in every group was created in
        // `build` as a descendant of `self.hwnd` and persists until destruction.
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

    /// Toggles field-map collapse state and resizes the window.
    pub fn toggle_field_map(&self) {
        let collapsed = !self.field_map_collapsed.get();
        self.field_map_collapsed.set(collapsed);
        // SAFETY: `self.hwnd` remains valid until `Drop`. `ID_FIELD_MAP_TOGGLE`
        // and controls touched by `apply_field_map_visibility` are child windows.
        unsafe {
            self.apply_field_map_visibility();
            if let Ok(btn) = dlg_item(self.hwnd, ID_FIELD_MAP_TOGGLE) {
                let text = field_map_toggle_label(collapsed);
                let _ = SetWindowTextW(btn, PCWSTR(wide(text).as_ptr()));
            }
        }
        self.ensure_room_for(self.field_map_bottom());
    }

    /// Records captured key code `vk`. Returns true if accepted.
    pub fn handle_capture_key(&self, vk: u16) -> bool {
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
        true
    }

    /// Populates Anki deck and model combos and field-map rows.
    pub fn populate_combos(&self, decks: &[String], models: &[String], fields: Vec<String>) {
        // SAFETY: `ID_ANKI_DECK` and `ID_ANKI_MODEL` are valid descendants of
        // `self.hwnd` created in `build`. `SendMessageW` copies text buffers.
        unsafe {
            if let Ok(deck) = dlg_item(self.hwnd, ID_ANKI_DECK) {
                fill_combo_if_changed(deck, decks);
            }
            if let Ok(model) = dlg_item(self.hwnd, ID_ANKI_MODEL) {
                fill_combo_if_changed(model, models);
            }
        }
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
        let needs_rebuild = {
            let rows = self.field_map_rows.borrow();
            let mut pending = self.pending_field_map.borrow_mut();
            begin_field_map_result(&fields, &rows, &mut pending)
        };
        if !needs_rebuild {
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
        self.ensure_room_for(self.field_map_bottom());
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
        let visible = self.current_tab.get() == 3 && !self.field_map_collapsed.get();
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

    /// Calculates bottom vertical coordinate of the field-map area.
    ///
    /// Expressed in window coordinates that match `bottom_y0`.
    fn field_map_bottom(&self) -> i32 {
        let n = self.pending_field_map.borrow().as_ref().map_or_else(
            || self.field_map_rows.borrow().len(),
            |pending| pending.fields.len(),
        );
        let page = if n == 0 || self.field_map_collapsed.get() {
            self.anki_static_bottom
        } else {
            self.anki_static_bottom + 20 + field_map_rows_needed(n) * ROW_H + 8
        };
        CONTENT_Y + page
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
        let f = self.font;
        let page = self.content;
        let y0 = self.anki_static_bottom;
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
        let f = self.font;
        let h = self.hwnd;
        let page = self.content;
        let y0 = self.anki_static_bottom;
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

    /// Adjusts window dimensions to fit page content.
    ///
    /// Dimensions do not decrease below initial layout sizes.
    fn ensure_room_for(&self, needed_bottom: i32) {
        let new_y0 = needed_bottom.max(self.bottom_y0);
        // SAFETY: `self.content` is a valid descendant of `self.hwnd` created
        // in `build`. `SWP_NOMOVE` keeps the origin, and `SWP_NOZORDER` keeps
        // the placement from `place_viewport`.
        unsafe {
            // Keeps page content visible.
            let _ = SetWindowPos(
                self.content,
                None,
                0,
                0,
                dpi_scale(self.hwnd, WIN_W),
                dpi_scale(self.hwnd, new_y0 - CONTENT_Y),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // Adjusts window size downward when content shrinks.
        self.fit_to(WIN_W, new_y0 + BOTTOM_H + PAD);
        // The resize changed the band dimensions.
        self.reset_scroll();
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
    /// `CreateWindowExW` requires outer window dimensions. `AdjustWindowRectEx`
    /// calculates border and caption offsets for the target monitor DPI.
    fn fit_to(&self, client_w: i32, client_h: i32) {
        // SAFETY: `self.hwnd` is a valid window handle. `rc` is local stack
        // storage that the call modifies. Failure leaves the current window size.
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

    /// Creates all window controls and returns the final vertical coordinate.
    unsafe fn build(&mut self, form: &SettingsForm, stale: &[String]) -> Result<i32> {
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

        // SAFETY: `h` is the window created by `open`. Child controls belong
        // to `h` or its internal panes. Parents are created before children.
        // Child window handles persist until destruction of `h`.
        unsafe {
            // Tabs and role lists require Common Controls initialization.
            let icex = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES,
            };
            let _ = InitCommonControlsEx(&icex);

            // ---- Tab control ----
            let tab = child(
                h,
                w!("SysTabControl32"),
                "",
                WS_TABSTOP | WS_CLIPSIBLINGS,
                PAD - 6,
                y,
                WIN_W - 2 * PAD,
                TAB_H,
                ID_TAB,
                f,
            )?;
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
            SendMessageW(
                tab,
                TCM_INSERTITEMW_MSG,
                Some(WPARAM(0)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            let mut t1 = wide("Dictionaries");
            item.psz_text = t1.as_mut_ptr();
            SendMessageW(
                tab,
                TCM_INSERTITEMW_MSG,
                Some(WPARAM(1)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            let mut t2 = wide("OCR / Debug");
            item.psz_text = t2.as_mut_ptr();
            SendMessageW(
                tab,
                TCM_INSERTITEMW_MSG,
                Some(WPARAM(2)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            let mut t3 = wide("Anki");
            item.psz_text = t3.as_mut_ptr();
            SendMessageW(
                tab,
                TCM_INSERTITEMW_MSG,
                Some(WPARAM(3)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            let mut t4 = wide("Plugins");
            item.psz_text = t4.as_mut_ptr();
            SendMessageW(
                tab,
                TCM_INSERTITEMW_MSG,
                Some(WPARAM(4)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            // The height stays 0 until the band height is known.
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
            // Without WS_EX_CONTROLPARENT, Tab skips every page.
            for pane in [self.viewport, self.content] {
                let ex = GetWindowLongW(pane, GWL_EXSTYLE) as u32 | WS_EX_CONTROLPARENT.0;
                SetWindowLongW(pane, GWL_EXSTYLE, ex as i32);
            }
            // `y` now counts from the page top, not from the window top.
            let page = self.content;
            y = 0;

            let group = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(
                    page,
                    w!("BUTTON"),
                    text,
                    WINDOW_STYLE(BS_GROUPBOX as u32),
                    PAD - 6,
                    y,
                    WIN_W - 2 * PAD,
                    height,
                    0,
                    f,
                )
            };
            // The same box, but WS_GROUP ends the group before it.
            let group_start = |text: &str, y: i32, height: i32| -> WinResult<HWND> {
                child(
                    page,
                    w!("BUTTON"),
                    text,
                    WINDOW_STYLE(BS_GROUPBOX as u32) | WS_GROUP,
                    PAD - 6,
                    y,
                    WIN_W - 2 * PAD,
                    height,
                    0,
                    f,
                )
            };
            let label = |text: &str, y: i32| -> WinResult<HWND> {
                child(
                    page,
                    w!("STATIC"),
                    text,
                    WINDOW_STYLE(0),
                    PAD,
                    y + 4,
                    LABEL_W,
                    ROW_H,
                    0,
                    f,
                )
            };

            // ---- Trigger ----
            gen.push(group("Trigger", y, ROW_H + ROW_GAP + ROW_H + 26)?);
            y += 20;
            // Live, Hold key, Toggle, and Press key share one radio group.
            // Press key shares the trigger key and runs one lookup per press.
            // Four 120-pixel radios at PAD, PAD + 130, PAD + 260, and PAD + 390
            // end at x=524 inside the 532-pixel group box.
            let live = child(
                page,
                w!("BUTTON"),
                "Live",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32) | WS_GROUP | WS_TABSTOP,
                PAD,
                y,
                120,
                ROW_H,
                ID_MODE_LIVE,
                f,
            )?;
            gen.push(live);
            let hold = child(
                page,
                w!("BUTTON"),
                "Hold key",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
                PAD + 130,
                y,
                120,
                ROW_H,
                ID_MODE_HOLD,
                f,
            )?;
            gen.push(hold);
            let toggle = child(
                page,
                w!("BUTTON"),
                "Toggle",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
                PAD + 260,
                y,
                120,
                ROW_H,
                ID_MODE_TOGGLE,
                f,
            )?;
            gen.push(toggle);
            let press = child(
                page,
                w!("BUTTON"),
                "Press key",
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
                PAD + 390,
                y,
                120,
                ROW_H,
                ID_MODE_PRESS,
                f,
            )?;
            gen.push(press);
            let is_live = matches!(form.mode, crate::config::TriggerMode::Live);
            let is_toggle = matches!(form.mode, crate::config::TriggerMode::Toggle);
            let is_press = matches!(form.mode, crate::config::TriggerMode::Press);
            let is_hold = !is_live && !is_toggle && !is_press;
            SendMessageW(
                live,
                BM_SETCHECK,
                Some(WPARAM(if is_live { 1 } else { 0 })),
                None,
            );
            SendMessageW(
                hold,
                BM_SETCHECK,
                Some(WPARAM(if is_hold { 1 } else { 0 })),
                None,
            );
            SendMessageW(
                toggle,
                BM_SETCHECK,
                Some(WPARAM(if is_toggle { 1 } else { 0 })),
                None,
            );
            SendMessageW(
                press,
                BM_SETCHECK,
                Some(WPARAM(if is_press { 1 } else { 0 })),
                None,
            );
            y += ROW_H + ROW_GAP;
            gen.push(label("Trigger key", y)?);
            let key_vk = crate::config::parse_trigger_key(&form.trigger_key).unwrap_or(0x10);
            CAPTURED_VK.with(|c| c.set(Some((h.0 as isize, key_vk))));
            let key_name = crate::config::trigger_key_name(key_vk);
            let key_btn = child(
                page,
                w!("BUTTON"),
                &key_name,
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_TRIGGER_KEY,
                f,
            )?;
            gen.push(key_btn);
            let _ = EnableWindow(key_btn, !is_live);
            y += ROW_H + 18;

            // ---- Popup ----
            // WS_GROUP ends the four-mode radio group above. Without it, the group runs
            // to the end of the window. Arrow keys then walk out of the Live, Hold key,
            // Toggle, and Press key buttons into the combos.
            gen.push(group_start(
                "Popup",
                y,
                7 * (ROW_H + ROW_GAP) + 4 * ROW_H + 30,
            )?);
            y += 20;
            gen.push(label("Theme", y)?);
            let theme = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_THEME,
                f,
            )?;
            gen.push(theme);
            for (i, name) in ["dark", "light"].iter().enumerate() {
                SendMessageW(
                    theme,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(name).as_ptr() as isize)),
                );
                if form.theme == *name {
                    SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(theme, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H + ROW_GAP;

            gen.push(label("Font", y)?);
            let fonts_hwnd = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                260,
                ID_FONT,
                f,
            )?;
            gen.push(fonts_hwnd);
            let mut families = japanese_font_families();
            // Keep a configured font when the system does not list it.
            // The code preserves the stored value until the user changes it.
            if !families.iter().any(|x| x == &form.font) {
                families.push(form.font.clone());
                families.sort();
            }
            for (i, name) in families.iter().enumerate() {
                SendMessageW(
                    fonts_hwnd,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(name).as_ptr() as isize)),
                );
                if name == &form.font {
                    SendMessageW(fonts_hwnd, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            self.fonts = families;
            y += ROW_H + ROW_GAP;

            gen.push(child(
                page,
                w!("BUTTON"),
                "Customize CSS\u{2026}",
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_CSS_EDITOR,
                f,
            )?);
            y += ROW_H + ROW_GAP;

            self.widths = numeric_choices(
                MAX_WIDTH_RANGE.0 as i64,
                MAX_WIDTH_RANGE.1 as i64,
                5,
                form.max_width_percent as i64,
            );
            gen.push(label("Max width (% of screen)", y)?);
            let mw = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_MAX_WIDTH,
                f,
            )?;
            gen.push(mw);
            fill_numeric(mw, &self.widths, form.max_width_percent as i64);
            y += ROW_H + ROW_GAP;

            self.heights = numeric_choices(
                MAX_HEIGHT_RANGE.0 as i64,
                MAX_HEIGHT_RANGE.1 as i64,
                5,
                form.max_height_percent as i64,
            );
            gen.push(label("Max height (% of screen)", y)?);
            let mh = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_MAX_HEIGHT,
                f,
            )?;
            gen.push(mh);
            fill_numeric(mh, &self.heights, form.max_height_percent as i64);
            y += ROW_H + ROW_GAP;

            self.summaries = numeric_choices(
                SUMMARY_RANGE.0 as i64,
                SUMMARY_RANGE.1 as i64,
                10,
                form.summary_chars as i64,
            );
            gen.push(label("Summary length (characters)", y)?);
            let sm = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_SUMMARY,
                f,
            )?;
            gen.push(sm);
            fill_numeric(sm, &self.summaries, form.summary_chars as i64);
            y += ROW_H + ROW_GAP + 4;

            let check = |text: &str, id: i32, on: bool, y: i32| -> WinResult<HWND> {
                let c = child(
                    page,
                    w!("BUTTON"),
                    text,
                    WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                    PAD,
                    y,
                    WIN_W - 2 * PAD - 20,
                    ROW_H,
                    id,
                    f,
                )?;
                SendMessageW(c, BM_SETCHECK, Some(WPARAM(if on { 1 } else { 0 })), None);
                Ok(c)
            };
            gen.push(check(
                "Box the word being defined",
                ID_HIGHLIGHT,
                form.highlight_match,
                y,
            )?);
            y += ROW_H;
            gen.push(check(
                "Scroll long entries with the wheel",
                ID_SCROLL,
                form.scroll_popup,
                y,
            )?);
            y += ROW_H;
            gen.push(check(
                "Auto-scroll while dragging at the popup edge",
                ID_EDGE_AUTOSCROLL,
                form.edge_autoscroll,
                y,
            )?);
            y += ROW_H;
            gen.push(check(
                "Show related words beside the popup",
                ID_SIDE_PANEL,
                form.side_panel,
                y,
            )?);
            y += ROW_H;
            gen.push(check(
                "Hide the popup from screen capture",
                ID_EXCLUDE,
                form.exclude_from_capture,
                y,
            )?);
            y += ROW_H + 18;

            // ---- Entry content ----
            // The render settings have a separate group, not four more rows
            // under Popup. The Linux window groups them in the same way.
            // These six fields define entry content. The rows above define
            // panel size. Both windows must keep every portable field.
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
            // The window shows one section for each role. Each section lists
            // every installed dictionary for that role and gives each row a
            // checkbox. Each section keeps its own order. A mixed archive
            // appears in every section that provides its data because the
            // enabled flag belongs to each role. Do not disable its frequency
            // data when the user clears its definitions
            // (ARCHITECTURE.md#dictionary-and-lookup).
            y = 0;
            let bx = WIN_W - PAD - BTN_W - 8;
            let list_w = bx - 2 * PAD + 4;
            let hint_w = WIN_W - 2 * PAD - 20;
            for (n, section) in SECTIONS.iter().enumerate() {
                if n > 0 {
                    y += GROUP_GAP;
                }
                // WS_GROUP ends the box before it, so only the first box can
                // go without it.
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
                    // The strategy row belongs to this list only. It changes
                    // the dictionary order for this list, not for the other two.
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
                    // If no ranking is selected, choose the first item.
                    // The default and read-back values stay consistent.
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

            // A rebuild uses the library only.
            if form.library_empty && !form.terms.is_empty() {
                dict.push(child(page, w!("STATIC"),
                    "chibipop is using a dictionary built outside the app. Adding or \
                     removing here rebuilds from this list only — import your original \
                     .zip files first.",
                    WINDOW_STYLE(0), PAD, y, hint_w, 44, 0, f)?);
                y += 48;
            }

            // Keep a configured name when no installed dictionary matches it.
            // The archive can have a new name or an unavailable path.
            // Do not rewrite the lists in either case
            // (ARCHITECTURE.md#dictionary-and-lookup).
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
            ocr.push(group("OCR / Debug", y, 16 * ROW_H + 38)?);
            y += 20;
            let plugins_root = crate::paths::beside_exe("plugins");
            let found = crate::plugin::discover::discover(&plugins_root);
            let mut engine_names = vec!["builtin".to_string()];
            engine_names.extend(discovered_text_providers(&found));
            // The combo still offers the configured engine.
            if form.engine != "builtin" && !engine_names.contains(&form.engine) {
                engine_names.push(form.engine.clone());
            }
            ocr.push(label("OCR engine", y)?);
            let engine = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W - BTN_W - 8,
                220,
                ID_ENGINE,
                f,
            )?;
            ocr.push(engine);
            for name in &engine_names {
                let shown = if name == "builtin" {
                    "Built-in (Windows OCR)"
                } else {
                    name
                };
                SendMessageW(
                    engine,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(shown).as_ptr() as isize)),
                );
            }
            let engine_idx = engine_names
                .iter()
                .position(|n| n == &form.engine)
                .unwrap_or(0);
            SendMessageW(engine, CB_SETCURSEL, Some(WPARAM(engine_idx)), None);
            self.engine_names = engine_names;
            let mut engine_dirs = HashMap::new();
            for (dir, parsed) in &found {
                if let Ok(m) = parsed {
                    if m.roles
                        .contains(&crate::plugin::manifest::Role::TextProvider)
                        && !engine_dirs.contains_key(&m.name)
                    {
                        engine_dirs.insert(m.name.clone(), dir.clone());
                    }
                }
            }
            self.engine_dirs = engine_dirs;
            let cfg_btn = child(
                page,
                w!("BUTTON"),
                "Configure…",
                WS_TABSTOP,
                FIELD_X + FIELD_W - BTN_W,
                y,
                BTN_W,
                ROW_H,
                ID_ENGINE_CONFIGURE,
                f,
            )?;
            ocr.push(cfg_btn);
            let _ = ShowWindow(cfg_btn, SW_HIDE);
            y += ROW_H;
            ocr.push(label("OCR language", y)?);
            let lang = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_OCR_LANG,
                f,
            )?;
            ocr.push(lang);
            let langs = language_choices(
                crate::text::ocr::installed_recognisers(),
                &form.ocr_language,
            );
            for (name, _) in &langs {
                SendMessageW(
                    lang,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(name).as_ptr() as isize)),
                );
            }
            if let Some(i) = language_index(&langs, &form.ocr_language) {
                SendMessageW(lang, CB_SETCURSEL, Some(WPARAM(i)), None);
            }
            self.ocr_langs = langs.into_iter().map(|(_, tag)| tag).collect();
            let _ = EnableWindow(lang, engine_idx == 0);
            y += ROW_H;
            ocr.push(child(
                page,
                w!("STATIC"),
                "Installed recognizers, plus any marked (not installed).",
                WINDOW_STYLE(0),
                PAD,
                y + 4,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                0,
                f,
            )?);
            y += ROW_H;
            self.passes = numeric_choices(
                PASSES_RANGE.0 as i64,
                PASSES_RANGE.1 as i64,
                1,
                form.max_ocr_passes as i64,
            );
            ocr.push(label("OCR passes per hover", y)?);
            let ps = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                160,
                ID_PASSES,
                f,
            )?;
            ocr.push(ps);
            fill_numeric(ps, &self.passes, form.max_ocr_passes as i64);
            y += ROW_H;
            ocr.push(child(
                page,
                w!("STATIC"),
                "1 = no tiling. Higher reads further ahead but can resolve the wrong character.",
                WINDOW_STYLE(0),
                PAD,
                y,
                WIN_W - 2 * PAD - 20,
                28,
                0,
                f,
            )?);
            y += 28;
            ocr.push(label("Capture width (px)", y)?);
            ocr.push(child(
                page,
                w!("EDIT"),
                &form.capture_width.to_string(),
                WS_TABSTOP | WS_BORDER,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_CAPTURE_W,
                f,
            )?);
            y += ROW_H;
            ocr.push(label("Capture height (px)", y)?);
            ocr.push(child(
                page,
                w!("EDIT"),
                &form.capture_height.to_string(),
                WS_TABSTOP | WS_BORDER,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_CAPTURE_H,
                f,
            )?);
            y += ROW_H;
            ocr.push(child(
                page,
                w!("STATIC"),
                "Vertical mode swaps these two values.",
                WINDOW_STYLE(0),
                PAD,
                y + 4,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                0,
                f,
            )?);
            y += ROW_H;
            ocr.push(check(
                "Prefer vertical text (manga, VN)",
                ID_PREFER_VERT,
                form.prefer_vertical,
                y,
            )?);
            y += ROW_H;
            ocr.push(check(
                "Scan alphanumeric text",
                ID_SCAN_ALNUM,
                form.scan_alphanumeric,
                y,
            )?);
            y += ROW_H;
            ocr.push(check(
                "Discard furigana from OCR text",
                ID_DISCARD_FURIGANA,
                form.discard_furigana,
                y,
            )?);
            y += ROW_H;
            let per_char = check(
                "Look up each character as you hover",
                ID_PER_CHAR,
                form.per_character_lookup,
                y,
            )?;
            ocr.push(per_char);
            let _ = EnableWindow(per_char, is_live);
            y += ROW_H;
            ocr.push(child(
                page,
                w!("STATIC"),
                "Live mode only. Off: the popup holds while the cursor stays on \
                 the matched word.",
                WINDOW_STYLE(0),
                PAD,
                y,
                WIN_W - 2 * PAD - 20,
                28,
                0,
                f,
            )?);
            y += 28;
            let scan = child(
                page,
                w!("BUTTON"),
                "Outline what each hover captured",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD,
                y,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                ID_SHOW_SCAN,
                f,
            )?;
            ocr.push(scan);
            SendMessageW(
                scan,
                BM_SETCHECK,
                Some(WPARAM(if form.show_scan_region { 1 } else { 0 })),
                None,
            );
            y += ROW_H;
            ocr.push(check(
                "Show which OCR engine is active",
                ID_ENGINE_LOG,
                form.show_engine_log,
                y,
            )?);
            y += ROW_H;
            ocr.push(check(
                "Show adapter log in status bar",
                ID_ADAPTER_LOG,
                form.show_adapter_log,
                y,
            )?);
            y += ROW_H + 18;
            let y_ocr = y;

            // ---- Anki (own tab) ----
            y = 0;
            ank.push(group("Anki", y, 16 * ROW_H + 34)?);
            y += 20;
            let anki_chk = child(
                page,
                w!("BUTTON"),
                "Enable Anki integration",
                WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                PAD,
                y,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                ID_ANKI_ENABLED,
                f,
            )?;
            ank.push(anki_chk);
            SendMessageW(
                anki_chk,
                BM_SETCHECK,
                Some(WPARAM(if form.anki_enabled { 1 } else { 0 })),
                None,
            );
            y += ROW_H;
            ank.push(check(
                "Show notification when a card is added",
                ID_NOTIFY_ON_ADD,
                form.notify_on_add,
                y,
            )?);
            y += ROW_H;
            ank.push(label("AnkiConnect URL", y)?);
            ank.push(child(
                page,
                w!("EDIT"),
                &form.anki_url,
                WS_TABSTOP | WS_BORDER,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_ANKI_URL,
                f,
            )?);
            y += ROW_H;
            ank.push(label("Deck", y)?);
            let deck = child(
                page,
                w!("COMBOBOX"),
                &form.anki_deck,
                WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                160,
                ID_ANKI_DECK,
                f,
            )?;
            ank.push(deck);
            SendMessageW(
                deck,
                WM_SETTEXT,
                None,
                Some(LPARAM(wide(&form.anki_deck).as_ptr() as isize)),
            );
            y += ROW_H;
            ank.push(label("Note type", y)?);
            let model = child(
                page,
                w!("COMBOBOX"),
                &form.anki_model,
                WINDOW_STYLE(CBS_DROPDOWN as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                160,
                ID_ANKI_MODEL,
                f,
            )?;
            ank.push(model);
            SendMessageW(
                model,
                WM_SETTEXT,
                None,
                Some(LPARAM(wide(&form.anki_model).as_ptr() as isize)),
            );
            y += ROW_H;
            ank.push(label("Shortcut key", y)?);
            let add_vk = crate::config::parse_trigger_key(&form.anki_add_key).unwrap_or(0x41);
            ANKI_CAPTURED_VK.with(|c| c.set(Some((h.0 as isize, add_vk))));
            let add_name = crate::config::trigger_key_name(add_vk);
            ank.push(child(
                page,
                w!("BUTTON"),
                &add_name,
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_ANKI_ADD_KEY,
                f,
            )?);
            y += ROW_H;
            ank.push(check(
                "Include screenshot when adding",
                ID_INCLUDE_SCREENSHOT,
                form.include_screenshot,
                y,
            )?);
            y += ROW_H;
            ank.push(label("Screenshot capture mode", y)?);
            let screenshot_mode = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                180,
                ID_SCREENSHOT_MODE,
                f,
            )?;
            ank.push(screenshot_mode);
            for (i, mode) in ScreenshotMode::ALL.iter().enumerate() {
                let text = mode.to_string();
                SendMessageW(
                    screenshot_mode,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(&text).as_ptr() as isize)),
                );
                if *mode == form.screenshot_capture_mode {
                    SendMessageW(screenshot_mode, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(screenshot_mode, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(screenshot_mode, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H;
            ank.push(child(
                page,
                w!("STATIC"),
                "Fixed modes save the first target. Hold Alt to switch region and window.",
                WINDOW_STYLE(0),
                PAD,
                y + 4,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                ID_SCREENSHOT_HINT,
                f,
            )?);
            y += ROW_H;
            let target_summary = screenshot_target_summary(form);
            ank.push(child(
                page,
                w!("STATIC"),
                &target_summary,
                WINDOW_STYLE(0),
                PAD,
                y + 4,
                WIN_W - 2 * PAD - 20,
                ROW_H,
                ID_SCREENSHOT_SUMMARY,
                f,
            )?);
            y += ROW_H;
            let reset = child(
                page,
                w!("BUTTON"),
                "Clear saved screenshot targets",
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_SCREENSHOT_RESET,
                f,
            )?;
            ank.push(reset);
            let has_target =
                form.screenshot_fixed_region.is_some() || form.screenshot_fixed_window.is_some();
            let _ = EnableWindow(reset, has_target);
            y += ROW_H;
            ank.push(check(
                "Include dictionary name",
                ID_INCLUDE_DICTIONARY_NAME,
                form.include_dictionary_name,
                y,
            )?);
            y += ROW_H;
            ank.push(check(
                "First dictionary only",
                ID_FIRST_DICT_ONLY,
                form.first_dict_only,
                y,
            )?);
            y += ROW_H;
            ank.push(label("Selection buttons", y)?);
            let selection_buttons = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                180,
                ID_SELECTION_BUTTONS,
                f,
            )?;
            ank.push(selection_buttons);
            for (i, (value, text)) in SELECTION_BUTTONS.iter().enumerate() {
                SendMessageW(
                    selection_buttons,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(text).as_ptr() as isize)),
                );
                if form.selection_buttons == *value {
                    SendMessageW(selection_buttons, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(selection_buttons, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(selection_buttons, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H;
            ank.push(label("Selection separator", y)?);
            let selection_separator = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                180,
                ID_SELECTION_SEPARATOR,
                f,
            )?;
            ank.push(selection_separator);
            for (i, (value, text)) in SELECTION_SEPARATORS.iter().enumerate() {
                SendMessageW(
                    selection_separator,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(text).as_ptr() as isize)),
                );
                if form.selection_separator == *value {
                    SendMessageW(selection_separator, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(selection_separator, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(selection_separator, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H;
            ank.push(label("Triple-click", y)?);
            let triple_click = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                180,
                ID_TRIPLE_CLICK,
                f,
            )?;
            ank.push(triple_click);
            for (i, (value, text)) in TRIPLE_CLICKS.iter().enumerate() {
                SendMessageW(
                    triple_click,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(text).as_ptr() as isize)),
                );
                if form.triple_click == *value {
                    SendMessageW(triple_click, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(triple_click, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(triple_click, CB_SETCURSEL, Some(WPARAM(1)), None);
            }
            y += ROW_H;
            ank.push(label("Sentence capture", y)?);
            let sentence_combo = child(
                page,
                w!("COMBOBOX"),
                "",
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                FIELD_X,
                y,
                FIELD_W,
                220,
                ID_SENTENCE_MODE,
                f,
            )?;
            ank.push(sentence_combo);
            for (i, (mode, text)) in SENTENCE_MODES.iter().enumerate() {
                SendMessageW(
                    sentence_combo,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(text).as_ptr() as isize)),
                );
                if form.sentence_mode == *mode {
                    SendMessageW(sentence_combo, CB_SETCURSEL, Some(WPARAM(i)), None);
                }
            }
            if SendMessageW(sentence_combo, CB_GETCURSEL, None, None).0 < 0 {
                SendMessageW(sentence_combo, CB_SETCURSEL, Some(WPARAM(0)), None);
            }
            y += ROW_H;
            let is_static = form.sentence_mode == SentenceMode::Static;
            ank.push(child(
                page,
                w!("STATIC"),
                "Region hotkey",
                WINDOW_STYLE(0),
                PAD,
                y + 4,
                LABEL_W,
                ROW_H,
                ID_STATIC_REGION_LABEL,
                f,
            )?);
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
            ank.push(child(
                page,
                w!("BUTTON"),
                &sr_label,
                WS_TABSTOP,
                FIELD_X,
                y,
                FIELD_W,
                ROW_H,
                ID_STATIC_REGION_KEY,
                f,
            )?);
            y += ROW_H;
            ank.push(check(
                "Show capture region outline",
                ID_SHOW_STATIC_OVERLAY,
                form.show_static_overlay,
                y,
            )?);
            y += ROW_H;
            ank.push(child(
                page,
                w!("STATIC"),
                "Tip: enable capture exclusion in General for best results",
                WINDOW_STYLE(0),
                PAD,
                y,
                WIN_W - 2 * PAD,
                ROW_H,
                ID_STATIC_CAPTURE_HINT,
                f,
            )?);
            if !is_static {
                for &id in &[
                    ID_STATIC_REGION_LABEL,
                    ID_STATIC_REGION_KEY,
                    ID_SHOW_STATIC_OVERLAY,
                    ID_STATIC_CAPTURE_HINT,
                ] {
                    if let Ok(c) = dlg_item(h, id) {
                        let _ = ShowWindow(c, SW_HIDE);
                    }
                }
            }
            y += ROW_H;
            ank.push(child(
                page,
                w!("BUTTON"),
                "Refresh",
                WS_TABSTOP,
                PAD,
                y,
                80,
                ROW_H,
                ID_ANKI_TEST,
                f,
            )?);
            ank.push(child(
                page,
                w!("STATIC"),
                "Click to load decks and field mappings from Anki",
                WINDOW_STYLE(0),
                PAD + 88,
                y + 2,
                WIN_W - 2 * PAD - 96,
                ROW_H,
                0,
                f,
            )?);
            y += ROW_H + 8 + GROUP_GAP;

            // ---- Field-map toggle ----
            let toggle_text = field_map_toggle_label(self.field_map_collapsed.get());
            ank.push(child(
                page,
                w!("BUTTON"),
                toggle_text,
                WS_TABSTOP,
                PAD,
                y,
                160,
                ROW_H,
                ID_FIELD_MAP_TOGGLE,
                f,
            )?);
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
                plug.push(child(
                    page,
                    w!("STATIC"),
                    &format!("No plugins found in {}.", plugins_root.display()),
                    WINDOW_STYLE(0),
                    PAD,
                    y,
                    WIN_W - 2 * PAD - 20,
                    36,
                    0,
                    f,
                )?);
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
                    plug.push(child(
                        page,
                        w!("STATIC"),
                        &row.label,
                        WINDOW_STYLE(0),
                        PAD,
                        ry + 4,
                        bx - PAD - 8,
                        ROW_H,
                        0,
                        f,
                    )?);
                    let chk = child(
                        page,
                        w!("BUTTON"),
                        "Enable",
                        WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                        bx,
                        ry,
                        BTN_W,
                        ROW_H,
                        ID_PLUGIN_ENABLE_BASE + idx,
                        f,
                    )?;
                    SendMessageW(
                        chk,
                        BM_SETCHECK,
                        Some(WPARAM(if row.checked { 1 } else { 0 })),
                        None,
                    );
                    let _ = EnableWindow(chk, row.can_enable);
                    plug.push(chk);
                    plug.push(child(
                        page,
                        w!("STATIC"),
                        &row.roles,
                        WINDOW_STYLE(0),
                        PAD,
                        ry + ROW_H + 4,
                        bx - PAD - 8,
                        ROW_H,
                        0,
                        f,
                    )?);
                    let status_y = ry + 2 * ROW_H;
                    plug.push(child(
                        page,
                        w!("STATIC"),
                        &row.status,
                        WINDOW_STYLE(0),
                        PAD,
                        status_y,
                        bx - PAD - 8,
                        PLUGIN_STATUS_H,
                        0,
                        f,
                    )?);
                    plug.push(child(
                        page,
                        w!("BUTTON"),
                        "Configure",
                        WS_TABSTOP,
                        bx,
                        status_y,
                        BTN_W,
                        ROW_H,
                        ID_PLUGIN_CONFIGURE_BASE + idx,
                        f,
                    )?);
                    y = ry + PLUGIN_ROW_H;
                }
            }
            y += 8 + GROUP_GAP;
            let y_plugins = y;

            // `y` counts from the window top from this point.
            // `place_bottom` positions these controls again.
            let bottom_y0 = y_general.max(y_dict).max(y_ocr).max(y_ank).max(y_plugins) + CONTENT_Y;

            // ---- Updates ----
            // The Updates box stays on `h`, not on the pane.
            child(
                h,
                w!("BUTTON"),
                "Updates",
                WINDOW_STYLE(BS_GROUPBOX as u32),
                PAD - 6,
                bottom_y0,
                WIN_W - 2 * PAD,
                ROW_H + 24,
                ID_UPDATES,
                f,
            )?;
            child(
                h,
                w!("BUTTON"),
                "Check for updates",
                WS_TABSTOP,
                PAD,
                bottom_y0 + BOTTOM_UPDATE_DY,
                136,
                ROW_H,
                ID_CHECK_UPDATE,
                f,
            )?;

            // ---- Apply / Cancel ----
            // The Apply / Cancel box also shows the progress line.
            let staged = form.has_staged();
            child(
                h,
                w!("EDIT"),
                apply_hint(self.apply_mode, staged),
                WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32) | WS_BORDER | WS_VSCROLL,
                PAD,
                bottom_y0 + BOTTOM_STATUS_DY,
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
                BOTTOM_APPLY_X,
                bottom_y0 + BOTTOM_BTN_DY,
                136,
                ROW_H + 4,
                ID_APPLY,
                f,
            )?;
            // Quit sits at the far left, not beside Apply.
            child(
                h,
                w!("BUTTON"),
                "Quit chibipop",
                WS_TABSTOP,
                PAD,
                bottom_y0 + BOTTOM_BTN_DY,
                116,
                ROW_H + 4,
                ID_QUIT,
                f,
            )?;

            // The tabs occupy this band.
            let band_h = bottom_y0 - CONTENT_Y;
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

            self.anki_static_bottom = y_ank;
            self.tab_heights = [y_general, y_dict, y_ocr, y_ank, y_plugins];
            self.bottom_y0 = bottom_y0;

            // The window starts on the General tab.
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
                    template.font.clone()
                } else {
                    self.fonts
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| template.font.clone())
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

            SettingsForm {
                mode: if checked(ID_MODE_PRESS) {
                    crate::config::TriggerMode::Press
                } else if checked(ID_MODE_TOGGLE) {
                    crate::config::TriggerMode::Toggle
                } else if checked(ID_MODE_HOLD) {
                    crate::config::TriggerMode::HoldKey
                } else {
                    crate::config::TriggerMode::Live
                },
                trigger_key,
                theme: theme.to_string(),
                font,
                max_width_percent: pick(
                    &self.widths,
                    ID_MAX_WIDTH,
                    template.max_width_percent as i64,
                ) as u8,
                max_height_percent: pick(
                    &self.heights,
                    ID_MAX_HEIGHT,
                    template.max_height_percent as i64,
                ) as u8,
                summary_chars: pick(&self.summaries, ID_SUMMARY, template.summary_chars as i64)
                    as usize,
                highlight_match: checked(ID_HIGHLIGHT),
                scroll_popup: checked(ID_SCROLL),
                edge_autoscroll: checked(ID_EDGE_AUTOSCROLL),
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
                max_ocr_passes: pick(&self.passes, ID_PASSES, template.max_ocr_passes as i64) as u8,
                prefer_vertical: checked(ID_PREFER_VERT),
                capture_width: px(ID_CAPTURE_W, template.capture_width),
                capture_height: px(ID_CAPTURE_H, template.capture_height),
                scan_alphanumeric: checked(ID_SCAN_ALNUM),
                discard_furigana: checked(ID_DISCARD_FURIGANA),
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
                include_screenshot: checked(ID_INCLUDE_SCREENSHOT),
                screenshot_capture_mode,
                screenshot_fixed_region: (!screenshot_reset_targets)
                    .then_some(template.screenshot_fixed_region)
                    .flatten(),
                screenshot_fixed_window: (!screenshot_reset_targets)
                    .then(|| template.screenshot_fixed_window.clone())
                    .flatten(),
                screenshot_reset_targets,
                ocr_clipboard_key,
                show_static_overlay: checked(ID_SHOW_STATIC_OVERLAY),
                include_dictionary_name: checked(ID_INCLUDE_DICTIONARY_NAME),
                first_dict_only: checked(ID_FIRST_DICT_ONLY),
                selection_buttons,
                selection_separator,
                triple_click,
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
            if let Some(f) = self.font {
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

    /// The X button quits standalone chibipop.
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
        assert!(field_map_toggle_label(true).ends_with('\u{25B6}'));
        assert!(field_map_toggle_label(false).ends_with('\u{25BC}'));
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
        form.ranking_strategy = RankingStrategy::Median;
        let window = SettingsWindow::open(&form, &[], ApplyMode::Standalone)
            .expect("opening the settings window");

        let back = window.read(&form);

        assert_eq!(form.terms, back.terms);
        assert_eq!(form.frequency, back.frequency);
        assert_eq!(form.pitch, back.pitch);
        assert_eq!(RankingStrategy::Median, back.ranking_strategy);
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
