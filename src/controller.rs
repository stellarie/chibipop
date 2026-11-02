//! The Controller is the hover and popup state machine.
//!
//! The platform bin sends `Event` values to the Controller and executes the
//! `Command` values that it returns. The core stores plain data in physical
//! pixels and makes no OS calls.

use std::collections::{HashMap, HashSet};

use crate::analysis::{TextKey, WordMap};

use crate::config::{ProfileSession, TriggerMode, TripleClick};
use crate::dict::gloss::{
    extent, leaf_text, leaves, RoleFilter, Separator,
};
use crate::geom::{in_sticky, PhysPoint, PhysRect, ScanRect};
use crate::present::{self, AnkiPopupState, OcrSurface, Presentation};
use crate::select::gesture::PressInput;
use crate::select::{
    entries, CardSelection, Coverage, Gesture, GestureEffect, GestureEnv, GestureInput, ItemSource,
    Selections, TextAddr,
};
use crate::text::layout::Orientation;

/// The cursor must move more than this many physical pixels on one axis.
const MOVEMENT_GATE_PX: i64 = 4;

/// A click chain expires after this many milliseconds.
///
/// Linux uses this value to stop its temporary gesture clock. Keep the
/// platform deadline and the core resolver on one duration.
pub const CLICK_CHAIN_MS: u64 = 500;

/// This four-pixel limit accounts for the `UPSCALE` value of 2.
const ANCHOR_JITTER_PX: i32 = 4;

/// The scroll distance for one wheel notch, in physical pixels.
const SCROLL_STEP_PX: i32 = 48;

/// The number of armed ticks before the Controller warns the user.
const ARM_WARN_TICKS: u32 = 250;

const MAX_SELECTED_TEXT_BYTES: usize = 64 * 1024;

/// A non-sentence result becomes stale when a newer non-sentence
/// `RequestId` exists. Sentence probes use unique IDs without replacing
/// hover results that are already active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// The action that a click on a region requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitAction {
    /// Expand collapsed row `i`.
    ExpandEntry(usize),
    /// Request a term lookup in the popup.
    ///
    /// A headword lookup uses one kanji character at a time.
    /// A glossary cross-reference lookup uses the full `?query=` target.
    DrillDown(String),
    /// Open an `http` or `https` citation in the user's browser.
    OpenUrl(String),
    /// Return to the previous history entry.
    Back,
    /// Select or clear all visible gloss content for Entry `e` in the top Card.
    ///
    /// The Entry header checkbox emits this action. `e` is the ordinal from
    /// [`crate::select::entries`].
    ToggleEntry(u32),
    ScrollThumb { travel: i32 },
}

/// A pointer button has a role instead of a physical code.
///
/// The platform bin maps button codes to this type. The Controller uses
/// `primary_additive` to decide whether a role adds to or replaces a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
}

/// The result of one lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum LookupOutcome {
    /// The lookup has no text or no result.
    Hide,
    /// The Controller logs this error and continues.
    Failed(String),
    /// The `scan` field is empty when debug output is off.
    Ready {
        presentation: Box<Presentation>,
        anchor: PhysRect,
        /// The axis along which the hold can grow.
        orientation: Orientation,
        /// The rectangle that matched the top card.
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
    },
    /// A Dictionary-only result for a kanji drill-down.
    DrillDown(Box<Presentation>),
    /// The sentence that an add-time probe read.
    ///
    /// `None` keeps the sentence from the hover-time presentation.
    Sentence(Option<String>),
}

/// The action that the tray menu selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    Quit,
}

/// An input event for the Controller.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    DismissRequested,
    PopupHover { local: PhysPoint, query: Option<String> },
    PopupHoverAt { depth: usize, local: PhysPoint, query: Option<String> },
    PopupLookupRequested {
        depth: usize, local: PhysPoint, query: Option<String>, session: ProfileSession,
    },
    PopupLookupCancelled,
    PopupActivated { depth: usize },
    PopupEntered { depth: usize },
    /// A dispatch tick with the live cursor and the Anki button height.
    /// The height is zero when the button is not visible.
    Tick { cursor: PhysPoint, button_h: i32 },
    /// A gesture-only tick from a platform without a dispatch timer.
    ///
    /// Linux sends these ticks only after pointer input. This keeps click chains
    /// and deferred plain-click clears alive. It does not wake the idle daemon.
    GestureTick,
    /// The number of complete wheel notches. Up is `+`.
    Scrolled { notches: i32 },
    /// A button press in popup-local coordinates. The platform bin performs
    /// the hit test because it owns the paint. `text` is the gloss address
    /// under the point from [`PopupScene::text_hit`], or `None` when the scene
    /// has no gloss text.
    ///
    /// [`PopupScene::text_hit`]: crate::ui::layout::PopupScene::text_hit
    PointerDown { local: PhysPoint, button: Button, hit: Option<HitAction>, text: Option<TextAddr> },
    /// A pointer move while a button is down. `local` can fall outside the
    /// popup during a drag.
    PointerMoved { local: PhysPoint, text: Option<TextAddr> },
    /// A button release.
    PointerUp { local: PhysPoint, button: Button },
    /// Word group boundaries for the top Card from Japanese analysis.
    /// The Controller ignores a stale `generation`.
    AnalysisReady { generation: u64, words: WordMap },
    /// A request from the Anki button or its hotkey.
    AddRequested,
    /// A request from the Back button or Escape.
    BackRequested,
    /// One configured lookup bind became active.
    LookupBindDown { bind_id: String, mode: TriggerMode, session: ProfileSession, pos: PhysPoint },
    /// One configured lookup bind became inactive.
    LookupBindUp { bind_id: String },
    /// Start a selection read.
    SelectedTextRequested { pos: PhysPoint, session: ProfileSession },
    /// Complete a selection read.
    SelectedTextReady { id: RequestId, text: Option<String>, bounds: Option<PhysRect> },
    /// A button press outside the popup while it is shown. The platform bin
    /// watches selected-text popups and `Press` mode. It carries no point because the
    /// Controller needs none. A hit inside the popup is `PointerDown`.
    PointerDownOutside,
    /// A cursor position that passed the movement gate.
    CursorMoved { pos: PhysPoint },
    /// The dwell deadline passed while the cursor stayed still
    /// (ARCHITECTURE.md#hover-cadence).
    DwellElapsed,
    /// The Worker returned a lookup result.
    LookupResult { id: RequestId, outcome: LookupOutcome },
    /// The platform bin placed the `ShowPopup` request.
    PopupPlaced { rect: PhysRect, content_h: i32, view_h: i32 },
    /// The platform bin could not place the `ShowPopup` request.
    PopupPlaceFailed,
    /// The result of one duplicate check. `None` means AnkiConnect refused
    /// the request.
    DupesChecked { generation: u64, session: ProfileSession, dupes: Option<HashSet<String>> },
    /// The result of one add-note request.
    NoteAdded { id: RequestId, expr: String, session: ProfileSession, failed: bool },
    /// The result of an add or verified overwrite request.
    NoteWritten { id: RequestId, expr: String, session: ProfileSession, status: NoteWriteStatus },
    /// The platform sent a new default profile configuration.
    ConfigReloaded { cfg: Box<ControllerConfig>, session: ProfileSession },
    /// Shared resources changed. The platform also reloads profile-independent worker state.
    SharedResourcesReloaded { cfg: Box<ControllerConfig>, session: ProfileSession },
    TrayAction(TrayAction),
    Quit,
}

/// Describes the visible result of one Anki write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteWriteStatus {
    Added,
    Updated,
    Failed,
}

/// The instruction that the Controller returns to the platform bin.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    PushPopup,
    RestorePopup { depth: usize },
    ClearPopupParents,
    /// Request a hover lookup at this physical point.
    ///
    /// `popup` is the core popup's on-screen rectangle while the lookup runs.
    /// It is `None` when no popup is shown.
    /// A live grab must mask this rectangle when the platform bin cannot
    /// exclude the surface from its OCR input
    /// (ARCHITECTURE.md#capture-and-masking).
    /// A platform bin that already excludes the popup ignores this field.
    RequestLookup { id: RequestId, point: PhysPoint, popup: Option<PhysRect>, session: ProfileSession },
    /// Read the full sentence around the hovered word for an Anki add.
    ///
    /// The bin hides the popup when `hide_popup` is true, sends
    /// `TriggerKind::Sentence` to the Worker, and restores the popup when the
    /// result arrives. Windows excludes its popup at the OS level and ignores
    /// the flag.
    RequestSentence {
        id: RequestId,
        anchor: PhysRect,
        orientation: Orientation,
        hide_popup: bool,
        session: ProfileSession,
    },
    /// Request a lookup from the Dictionary data only.
    RequestDrillDown { id: RequestId, text: String, session: ProfileSession },
    /// Read application selection.
    ReadSelectedText { id: RequestId, session: ProfileSession },
    OpenSentenceSearch { text: String, session: ProfileSession },
    /// Send the current settings to the Worker.
    RequestReload { id: RequestId },
    /// Measure and place the popup. Show and paint it.
    /// Return the result as `PopupPlaced`.
    ShowPopup {
        presentation: Box<Presentation>,
        anchor: PhysRect,
        scroll: i32,
        show_back: bool,
    },
    /// Repaint the popup in its current rectangle.
    RepaintPopup { scroll: i32, show_back: bool },
    /// Hide the popup, the scan overlay, and the Anki button.
    HidePopup,
    ShowScanOverlay { rects: Vec<ScanRect> },
    /// Update the Anki button's placement, paint, and visibility.
    SyncAnkiButton,
    SetScrollArmed(bool),
    SetClickArmed(bool),
    SetAddArmed(bool),
    SetBackArmed(bool),
    /// Discard the stored wheel delta.
    DiscardScroll,
    /// The cursor position in popup-local coordinates.
    /// The platform bin tests this position and shows the hand cursor on a
    /// hit.
    SetCursorShape { local: PhysPoint, scroll: i32 },
    CheckDupes { generation: u64, exprs: Vec<String>, session: ProfileSession },
    AddNote { id: RequestId, expr: String, fields: HashMap<String, String>, session: ProfileSession },
    /// Write a lookup log line when configuration enables it.
    LogLookup { headword: String, match_len: usize },
    WarnLookupFailed(String),
    WarnScrollCaptured { seconds: u32 },
    /// Open a glossary citation in the desktop browser.
    /// Accept only `http` or `https`. `layout::link_action` allow-lists the
    /// scheme because the URL comes from a dictionary file that chibipop did
    /// not write.
    OpenUrl(String),
    /// Analyze the text leaves of the top Card on the analysis thread.
    /// The reply arrives as [`Event::AnalysisReady`] with the same generation.
    RequestAnalysis { generation: u64, texts: Vec<(TextKey, String)> },
    /// A selection drag starts (`true`) or ends (`false`). The platform bin
    /// forwards pointer moves outside the popup while this value is `true`.
    SetDragging(bool),
    OpenSettings,
    Exit,
}

/// Settings that the Controller reads from its configuration. Reload refreshes
/// these settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConfig {
    pub sub_popups: bool,
    pub selected_text_sentence_search: bool,
    pub trigger_mode: TriggerMode,
    pub per_character_lookup: bool,
    pub scroll_popup: bool,
    pub anki_enabled: bool,
    pub overwrite_duplicates: bool,
    /// Read the full sentence on add (`SentenceMode::Sentence`).
    pub sentence_probe: bool,
    /// Include each Dictionary name in both Anki glossary fields.
    pub include_dictionary_name: bool,
    /// Send only the first Dictionary's glossary block to Anki.
    /// This matches upstream 0.9.x "first dict only".
    /// A Card selection overrides this setting.
    pub first_dict_only: bool,
    pub summary_chars: usize,
    pub log_lookups: bool,
    /// The platform bin dispatch interval, in milliseconds.
    pub tick_ms: u32,
    /// The editorial roles that the popup shows. A Card selection addresses
    /// only this content.
    pub roles: RoleFilter,
    /// Scroll the popup while a selection drag reaches its edge.
    /// If `scroll_popup` is `false`, this option is also disabled.
    pub edge_autoscroll: bool,
    /// The physical primary button adds to a selection.
    /// When `primary_additive` is `false`, the primary button replaces the
    /// selection. The secondary button adds to it.
    pub primary_additive: bool,
    /// The separator between disjoint selected fragments in one container.
    pub separator: Separator,
    /// What a triple-click selects.
    pub triple_click: TripleClick,
}

impl ControllerConfig {
    pub fn for_search(config: &crate::config::ResolvedConfig) -> Self {
        Self {
            sub_popups: config.popup.sub_popups,
            selected_text_sentence_search: false,
            trigger_mode: TriggerMode::Press,
            per_character_lookup: false,
            scroll_popup: config.popup.scroll_popup,
            anki_enabled: false,
            overwrite_duplicates: false,
            sentence_probe: false,
            include_dictionary_name: config.anki.include_dictionary_name,
            first_dict_only: config.anki.first_dict_only,
            summary_chars: config.popup.summary_chars,
            log_lookups: config.debug.show_lookup_log,
            tick_ms: 20,
            roles: config.popup.render_settings().roles,
            edge_autoscroll: config.popup.edge_autoscroll,
            primary_additive: config.anki.selection_buttons == crate::config::SelectionButtons::PrimaryAdditive,
            separator: config.anki.selection_separator.into(),
            triple_click: config.anki.triple_click,
        }
    }
    pub fn for_profile(config: &crate::config::ResolvedConfig) -> Self {
        Self {
            sub_popups: config.popup.sub_popups,
            selected_text_sentence_search: config.actions.search.selected_opens_sentence_search,
            trigger_mode: config.trigger.mode,
            per_character_lookup: config.trigger.per_character_lookup,
            scroll_popup: config.popup.scroll_popup,
            anki_enabled: config.anki.enabled,
            overwrite_duplicates: config.anki.overwrite_duplicates,
            sentence_probe: config.anki.sentence_mode == crate::config::SentenceMode::Sentence,
            include_dictionary_name: config.anki.include_dictionary_name,
            first_dict_only: config.anki.first_dict_only,
            summary_chars: config.popup.summary_chars,
            log_lookups: config.debug.show_lookup_log,
            tick_ms: 20,
            roles: config.popup.render_settings().roles,
            edge_autoscroll: config.popup.edge_autoscroll,
            primary_additive: config.anki.selection_buttons == crate::config::SelectionButtons::PrimaryAdditive,
            separator: config.anki.selection_separator.into(),
            triple_click: config.anki.triple_click,
        }
    }
}

/// This freeze applies only in Live mode.
pub fn per_char_freeze(on: bool, mode: TriggerMode) -> bool {
    on && matches!(mode, TriggerMode::Live)
}

/// The hold for the matched span and the hold for one character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldRects {
    pub hold: PhysRect,
    pub hold_char: PhysRect,
}

pub fn hold_regions(
    anchor: PhysRect,
    matched: Option<PhysRect>,
    orientation: Orientation,
) -> HoldRects {
    HoldRects {
        hold: hold_region(anchor, matched, orientation),
        hold_char: hold_region(anchor, None, orientation),
    }
}

/// Match the selected axis and provide extra space on the other axis.
pub fn hold_region(
    anchor: PhysRect,
    matched: Option<PhysRect>,
    orientation: Orientation,
) -> PhysRect {
    let span = matched.unwrap_or(anchor);
    match orientation {
        Orientation::Horizontal => PhysRect {
            x: span.x,
            y: anchor.y - anchor.h / 2,
            w: span.w,
            h: anchor.h * 2,
        },
        Orientation::Vertical => PhysRect {
            x: anchor.x - anchor.w / 2,
            y: span.y,
            w: anchor.w * 2,
            h: span.h,
        },
    }
}

/// Build the add-note payload.
///
/// `expr` uses `written` when present and `reading` otherwise.
/// Include blocks from the first Dictionary only when `first_dict_only` is true.
/// The payload includes Dictionary headings when `include_dictionary_name` is
/// `true`.
/// A non-empty Card selection always includes every selected Entry.
/// The caller passes an empty Card selection only for the whole-Card path.
/// Include the captured sentence when it exists.
///
/// The Controller and the platform bins use one rule for screenshot fields.
/// If no top card exists, return an empty `expr` and no fields.
pub fn note_payload(
    p: &Presentation,
    first_dict_only: bool,
    include_dictionary_name: bool,
    selection: &CardSelection,
    separator: Separator,
) -> (String, HashMap<String, String>) {
    let Some(card) = p.top.as_ref() else {
        return (String::new(), HashMap::new());
    };
    let expr = card
        .written
        .as_deref()
        .or(card.reading.as_deref())
        .unwrap_or("")
        .to_string();
    let mut fields = if selection.is_empty() {
        let blocks_to_send = if first_dict_only {
            &card.blocks[..1.min(card.blocks.len())]
        } else {
            &card.blocks[..]
        };
        crate::anki::fields_from_card(card, blocks_to_send, include_dictionary_name)
    } else {
        crate::anki::fields_from_selection(
            card,
            selection,
            separator,
            include_dictionary_name,
        )
    };
    if let Some(sentence) = &p.sentence {
        let surface = p.surface.as_ref().and_then(OcrSurface::as_str);
        fields.insert("sentence".to_string(), bold_surface(sentence, surface));
    }
    (expr, fields)
}

/// Wrap the first `surface` inside `sentence` in `<b>`.
///
/// An Anki field holds HTML, so `<b>` renders bold and stays searchable as the
/// plain characters. The match is the first occurrence because the probe gives
/// no offset. A sentence without the surface, for example after an OCR
/// difference between the hover read and the probe read, stays plain.
fn bold_surface(sentence: &str, surface: Option<&str>) -> String {
    let Some(surface) = surface.filter(|s| !s.is_empty()) else {
        return sentence.to_string();
    };
    let Some(at) = sentence.find(surface) else {
        return sentence.to_string();
    };
    let mut out = String::with_capacity(sentence.len() + 7);
    out.push_str(&sentence[..at]);
    out.push_str("<b>");
    out.push_str(surface);
    out.push_str("</b>");
    out.push_str(&sentence[at + surface.len()..]);
    out
}

/// State saved before a drill-down so Back can restore it.
#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    source: RootSource,
    session: ProfileSession,
    hovered: Option<String>,
    anchor: PhysRect,
    orientation: Orientation,
    hold: PhysRect,
    hold_char: PhysRect,
    presentation: Presentation,
    anki: AnkiPopupState,
    anki_write: Option<RequestId>,
    scroll: i32,
    generation: u64,
    selection: Selections,
    gesture: Gesture,
    analysis: Option<(u64, WordMap)>,
    analysis_stale: bool,
    analysis_generation: u64,
    pressed_link: Option<HitAction>,
    last_drag_point: Option<PhysPoint>,
    /// The add-time sentence read that belongs to this popup.
    pending_sentence: Option<RequestId>,
    placed: Option<Placed>,
}

/// The measured geometry of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    popup: PhysRect,
    /// The natural content height before the clamp.
    content_h: i32,
    /// The popup view height.
    view_h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollDrag {
    start_y: i32,
    start_scroll: i32,
    travel: i32,
}

/// The note payload authorized when an add-time sentence probe starts.
///
/// Popup state can change while the Worker reads the sentence. Keep this
/// payload independent so the result cannot retarget a newer Card.
#[derive(Debug, Clone, PartialEq)]
struct PendingSentence {
    expr: String,
    fields: HashMap<String, String>,
    surface: Option<String>,
    session: ProfileSession,
}

/// The reason that a `ShowPopup` request is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceKind {
    /// The first popup for a new hover.
    Fresh,
    /// A popup after a drill-down.
    DrillDown,
    /// The same popup with new content.
    Reshow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    Ocr,
    SelectedText,
}

struct RootTransition {
    source: RootSource,
    session: ProfileSession,
    replace_chain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedTextState {
    Capturing { id: RequestId, pos: PhysPoint, session: ProfileSession },
    LookingUp { id: RequestId, anchor: PhysRect, text: String, session: ProfileSession },
}
/// State for one shown popup.
#[derive(Debug, Clone, PartialEq)]
struct Surface {
    source: RootSource,
    session: ProfileSession,
    hovered: Option<String>,
    /// The rectangle of the hovered glyph.
    anchor: PhysRect,
    /// The axis along which the hold can grow.
    orientation: Orientation,
    /// The rectangle where the cursor can move without a new lookup.
    hold: PhysRect,
    /// The hold rectangle for one character.
    hold_char: PhysRect,
    presentation: Presentation,
    anki: AnkiPopupState,
    anki_write: Option<RequestId>,
    /// The drill-down history stack.
    history: Vec<HistoryEntry>,
    /// The content offset. Zero places the content at the top.
    scroll: i32,
    scroll_drag: Option<ScrollDrag>,
    /// The generation that guards against stale duplicate results.
    generation: u64,
    /// The selected ranges for every Card in the `Presentation`.
    selection: Selections,
    /// The current click chain and drag state.
    gesture: Gesture,
    /// The Japanese analysis result for the current generation.
    analysis: Option<(u64, WordMap)>,
    /// Whether a `Reshow` must refresh Japanese analysis and gesture state.
    ///
    /// Content changes set this flag before placement. A marker-only `Reshow`
    /// can retain the current Japanese analysis and active gesture.
    analysis_stale: bool,
    /// The generation that the current analysis request uses.
    analysis_generation: u64,
    /// The last deferred link action from a primary pointer press.
    pressed_link: Option<HitAction>,
    /// The last popup-local pointer point during a drag.
    last_drag_point: Option<PhysPoint>,
    /// The add-time sentence read and its authorized note payload.
    pending_sentence: Option<RequestId>,
    placed: Option<Placed>,
}

impl HistoryEntry {
    fn restore(self, surface: &mut Surface) {
        surface.source = self.source;
        surface.session = self.session;
        surface.hovered = self.hovered;
        surface.anchor = self.anchor;
        surface.orientation = self.orientation;
        surface.hold = self.hold;
        surface.hold_char = self.hold_char;
        surface.presentation = self.presentation;
        surface.anki = self.anki;
        surface.anki_write = self.anki_write;
        surface.scroll = self.scroll;
        surface.generation = self.generation;
        surface.selection = self.selection;
        surface.gesture = self.gesture;
        surface.analysis = self.analysis;
        surface.analysis_stale = self.analysis_stale;
        surface.analysis_generation = self.analysis_generation;
        surface.pressed_link = self.pressed_link;
        surface.last_drag_point = self.last_drag_point;
        surface.pending_sentence = self.pending_sentence;
        surface.placed = self.placed;
    }
}

/// State that the platform bin can read.
pub struct PopupView<'a> {
    pub popup: PhysRect,
    /// The hovered glyph rectangle for actions that use the movement gate.
    pub anchor: PhysRect,
    pub scroll: i32,
    pub content_h: i32,
    pub view_h: i32,
    pub presentation: &'a Presentation,
    pub anki: &'a AnkiPopupState,
    pub session: &'a ProfileSession,
    pub show_back: bool,
    pub selection: &'a Selections,
}

/// The Controller state machine for hover and popup events.
pub struct Controller {
    hover_buttons: u8,
    hover_candidate: Option<(String, PhysPoint, u64)>,
    hover_request: Option<(RequestId, PhysPoint, ProfileSession)>,
    popup_request: Option<(RequestId, PhysPoint, ProfileSession)>,
    cfg: ControllerConfig,
    cfg_session: ProfileSession,
    future_cfg: ControllerConfig,
    future_session: ProfileSession,
    active_bind: Option<ActiveBind>,
    chain_bind: Option<ActiveBind>,
    request_session: Option<LookupRequest>,
    pending_sentences: HashMap<RequestId, PendingSentence>,
    pending_writes: HashMap<RequestId, ProfileSession>,
    /// Popup states hidden by nested hover links.
    parents: Vec<Surface>,
    pointer_depth: Option<usize>,
    /// The current popup, when one exists.
    surface: Option<Surface>,
    /// Scan overlay rectangles held until popup placement.
    pending_scan: Vec<ScanRect>,
    /// The kind of popup placement that awaits a result.
    awaiting: Option<PlaceKind>,
    /// The latest cursor point that awaits the popup rectangle.
    pending_cursor: Option<(PhysPoint, ProfileSession, bool)>,
    /// The latest cursor point that passed the movement gate.
    last_accepted: Option<PhysPoint>,
    /// The point of the newest lookup. The dwell re-check uses this point.
    last_dispatch: Option<PhysPoint>,
    /// The point of the lookup that produced the
    /// shown popup.
    shown_dispatch: Option<PhysPoint>,
    /// Whether the active trigger is held or latched.
    trigger_held: bool,
    /// The Anki button height from the latest tick.
    button_h: i32,
    /// The count of consecutive ticks while the scroll action is armed.
    armed_ticks: u32,
    next_id: u64,
    /// The latest request ID includes add-time sentence reads.
    ///
    /// Hover-result staleness uses `latest_lookup` because a sentence read
    /// must not replace a hover result that was already active.
    latest: RequestId,
    /// The newest request that replaces non-sentence lookup outcomes.
    ///
    /// A sentence read uses `next_request` for a unique ID but leaves this
    /// marker unchanged.
    latest_lookup: RequestId,
    selected_text: Option<SelectedTextState>,
    generation: u64,
    /// The monotonic dispatch tick that times pointer gestures.
    clock: u64,
}
#[derive(Debug, Clone, PartialEq)]
struct LookupRequest {
    id: RequestId,
    session: ProfileSession,
    replace_chain: bool,
}
#[derive(Debug, Clone, PartialEq)]
struct ActiveBind {
    id: String,
    mode: TriggerMode,
    session: ProfileSession,
}


impl Controller {
    pub fn new(cfg: ControllerConfig, session: ProfileSession) -> Self {
        Self {
            hover_buttons: 0,
            pointer_depth: None,
            parents: Vec::new(),
            hover_candidate: None,
            hover_request: None,
            popup_request: None,
            cfg: cfg.clone(),
            cfg_session: session.clone(),
            future_cfg: cfg,
            future_session: session,
            active_bind: None,
            chain_bind: None,
            request_session: None,
            pending_sentences: HashMap::new(),
            pending_writes: HashMap::new(),
            surface: None,
            pending_scan: Vec::new(),
            awaiting: None,
            pending_cursor: None,
            last_accepted: None,
            last_dispatch: None,
            shown_dispatch: None,
            trigger_held: false,
            button_h: 0,
            armed_ticks: 0,
            next_id: 0,
            latest: RequestId(0),
            latest_lookup: RequestId(0),
            selected_text: None,
            generation: 0,
            clock: 0,
        }
    }

    pub fn trigger_mode(&self) -> TriggerMode {
        self.active_bind
            .as_ref()
            .or(self.chain_bind.as_ref())
            .map_or(self.cfg.trigger_mode, |bind| bind.mode)
    }

    pub fn active_bind_id(&self) -> Option<&str> {
        self.active_bind.as_ref().map(|bind| bind.id.as_str())
    }

    pub fn session_at(&self, depth: usize) -> Option<&ProfileSession> {
        if depth == self.parents.len() {
            return self.surface.as_ref().map(|surface| &surface.session);
        }
        self.parents.get(depth).map(|surface| &surface.session)
    }

    /// The shown popup when its rectangle is known.
    pub fn popup(&self) -> Option<PopupView<'_>> {
        let s = self.surface.as_ref()?;
        let p = s.placed.as_ref()?;
        Some(PopupView {
            popup: p.popup,
            anchor: s.anchor,
            scroll: s.scroll,
            content_h: p.content_h,
            view_h: p.view_h,
            presentation: &s.presentation,
            anki: &s.anki,
            session: &s.session,
            show_back: !s.history.is_empty() || !self.parents.is_empty(),
            selection: &s.selection,
        })
    }

    pub fn selected_text_popup(&self) -> bool {
        self.surface.as_ref().is_some_and(|surface| surface.source == RootSource::SelectedText)
    }

    pub fn watches_outside_clicks(&self) -> bool {
        self.selected_text_active()
            || (self.surface.is_some()
                && (self.trigger_mode() != TriggerMode::Live || !self.hides_on_cursor_exit()))
    }

    /// Whether the bin must consume the outside
    /// press that hides the popup.
    pub fn swallows_outside_clicks(&self) -> bool {
        self.watches_outside_clicks() && (!self.selected_text_active() || !self.hides_on_cursor_exit())
    }

    /// Whether a cursor miss hides the popup.
    /// The root popup's profile decides.
    fn hides_on_cursor_exit(&self) -> bool {
        self.parents.first().or(self.surface.as_ref())
            .is_none_or(|surface| surface.session.config().popup.hide_on_cursor_exit)
    }

    /// Whether a miss for `request` keeps the
    /// popup. Key lookups never keep it.
    fn keeps_popup_on_miss(&self, request: &LookupRequest) -> bool {
        !request.replace_chain && self.surface.is_some() && !self.hides_on_cursor_exit()
    }

    pub fn popup_depth(&self) -> usize {
        self.parents.len()
    }

    pub fn parent_popup(&self, depth: usize) -> Option<PopupView<'_>> {
        let s = self.parents.get(depth)?;
        let p = s.placed?;
        Some(PopupView {
            popup: p.popup, anchor: s.anchor, scroll: s.scroll,
            content_h: p.content_h, view_h: p.view_h,
            presentation: &s.presentation, anki: &s.anki,
            session: &s.session,
            show_back: depth > 0 || !s.history.is_empty(), selection: &s.selection,
        })
    }

    pub fn popup_at(&self, point: PhysPoint) -> Option<usize> {
        if self.shown_popup().is_some_and(|rect| rect.contains(point)) {
            return Some(self.parents.len());
        }
        self.parents.iter().rposition(|s| s.placed.is_some_and(|p| p.popup.contains(point)))
    }

    pub fn popup_rects(&self) -> Vec<PhysRect> {
        self.parents.iter().chain(self.surface.iter()).filter_map(|s| s.placed.map(|p| p.popup)).collect()
    }

    fn cancel_hover(&mut self) {
        self.hover_candidate = None;
        if self.hover_request.take().is_some() {
            self.next_lookup_request();
        }
    }

    fn popup_hover(&mut self, local: PhysPoint, query: Option<String>) -> Vec<Command> {
        if self.popup_request.is_some() { return Vec::new(); }
        let Some(sub_popups) = self.surface_config().map(|config| config.popup.sub_popups) else {
            return Vec::new();
        };
        if !sub_popups {
            self.cancel_hover();
            return Vec::new();
        }
        let Some(s) = self.surface.as_ref() else { return Vec::new() };
        if s.placed.is_none() || s.last_drag_point.is_some() || self.hover_buttons != 0 {
            return Vec::new();
        }
        let query = query.filter(|q| !q.is_empty());
        if s.hovered == query {
            return Vec::new();
        }
        self.cancel_hover();
        self.surface.as_mut().expect("surface exists").hovered = query.clone();
        if let Some(query) = query {
            self.next_lookup_request();
            let delay = 300u64.div_ceil(u64::from(self.future_cfg.tick_ms.max(1)));
            self.hover_candidate = Some((query, local, self.clock.saturating_add(delay)));
        }
        Vec::new()
    }

    fn popup_hover_at(&mut self, depth: usize, local: PhysPoint, query: Option<String>) -> Vec<Command> {
        let enabled = if depth == self.parents.len() {
            self.surface_config().is_some_and(|config| config.popup.sub_popups)
        } else {
            self.parents.get(depth)
                .is_some_and(|parent| parent.session.config().popup.sub_popups)
        };
        if !enabled { return Vec::new(); }
        if depth == self.parents.len() { return self.popup_hover(local, query); }
        let Some(parent) = self.parents.get(depth) else { return Vec::new() };
        if self.hover_buttons != 0 || query.as_ref().is_none_or(String::is_empty) || parent.hovered == query {
            return Vec::new();
        }
        let Some(placed) = parent.placed else { return Vec::new() };
        let point = PhysPoint {
            x: placed.popup.x.saturating_add(local.x),
            y: placed.popup.y.saturating_add(local.y),
        };
        if self.parents.get(depth + 1).or(self.surface.as_ref()).is_some_and(|child| {
            child.placed.is_none_or(|placed| in_sticky(point, child.hold, child.hold, placed.popup))
        }) {
            return Vec::new();
        }
        let mut out = self.enter_parent(depth);
        out.extend(self.popup_hover(local, query));
        out
    }

    fn hover_tick(&mut self) -> Vec<Command> {
        let Some(sub_popups) = self.surface_config().map(|config| config.popup.sub_popups) else {
            return Vec::new();
        };
        if !sub_popups {
            self.cancel_hover();
            return Vec::new();
        }
        if self.hover_candidate.as_ref().is_none_or(|(_, _, deadline)| self.clock < *deadline) {
            return Vec::new();
        }
        let Some((text, local, _)) = self.hover_candidate.take() else { return Vec::new() };
        if self.surface.as_ref().is_none_or(|s| s.placed.is_none() || s.last_drag_point.is_some()) {
            return Vec::new();
        }
        let session = self.surface.as_ref().expect("surface checked above").session.nested();
        let id = self.next_lookup_request();
        self.hover_request = Some((id, local, session.clone()));
        vec![Command::RequestDrillDown { id, text, session }]
    }

    fn popup_lookup(
        &mut self,
        depth: usize,
        local: PhysPoint,
        query: Option<String>,
        session: ProfileSession,
    ) -> Vec<Command> {
        if self.hover_buttons != 0 { return Vec::new(); }
        let surface = if depth == self.parents.len() {
            self.surface.as_ref()
        } else {
            self.parents.get(depth)
        };
        if surface.is_none_or(|s| s.placed.is_none() || s.last_drag_point.is_some()) {
            return Vec::new();
        }
        let Some(text) = query.filter(|text| !text.is_empty()) else { return Vec::new() };
        let mut out = self.enter_parent(depth);
        self.cancel_hover();
        let id = self.next_lookup_request();
        self.pointer_depth = Some(depth);
        let surface = self.surface.as_mut().expect("source popup exists");
        if surface.hovered.as_deref() != Some(text.as_str()) {
            surface.hovered = Some(text.clone());
        }
        self.popup_request = Some((id, local, session.clone()));
        out.push(Command::RequestDrillDown { id, text, session });
        out
    }

    fn enter_parent(&mut self, depth: usize) -> Vec<Command> {
        if depth >= self.parents.len() {
            return Vec::new();
        }
        self.cancel_hover();
        self.next_lookup_request();
        self.parents.truncate(depth + 1);
        self.surface = self.parents.pop();
        self.hover_buttons = 0;
        self.pointer_depth = Some(depth);
        self.awaiting = None;
        self.pending_cursor = None;
        let mut out = vec![Command::RestorePopup { depth }, Command::SetDragging(false), Command::SyncAnkiButton];
        let session = self.active_bind.as_ref().map(|bind| &bind.session)
            .or_else(|| self.chain_bind.as_ref().map(|bind| &bind.session))
            .or_else(|| self.surface.as_ref().map(|surface| &surface.session))
            .unwrap_or(&self.future_session)
            .clone();
        self.set_profile_config(&session);
        let gesture_commands = self.run_gesture(GestureInput::Tick { tick: self.clock });
        if gesture_commands.is_empty() {
            if let Some(s) = self.surface.as_ref() {
                out.push(self.repaint(s.scroll));
            }
        } else {
            out.extend(gesture_commands);
        }
        out.push(Command::SetBackArmed(self.has_history()));
        let (anki_enabled, roles) = self
            .surface_config()
            .map(|config| (config.anki.enabled, config.popup.render_settings().roles))
            .expect("restored parent surface");
        if anki_enabled {
            if let Some(s) = self.surface.as_mut().filter(|s| s.analysis.is_none()) {
                self.generation = self.generation.wrapping_add(1);
                s.analysis_generation = self.generation;
                let mut texts = Vec::new();
                if let Some(card) = &s.presentation.top {
                    for (entry, gloss) in entries(card) {
                        for leaf in leaves(&gloss.doc, roles) {
                            let text = leaf_text(&gloss.doc, leaf.path);
                            if !text.is_empty() { texts.push(((entry, leaf.path), text.to_string())); }
                        }
                    }
                }
                if !texts.is_empty() { out.push(Command::RequestAnalysis { generation: self.generation, texts }); }
            }
        }
        out
    }

    fn popup_entered(&mut self, depth: usize) -> Vec<Command> {
        if self.hover_buttons != 0 { return Vec::new(); }
        if self.pointer_depth == Some(depth) { return Vec::new(); }
        self.pointer_depth = (depth <= self.parents.len()).then_some(depth);
        self.enter_parent(depth)
    }

    fn push_popup(&mut self, mut presentation: Presentation, local: PhysPoint, session: ProfileSession, explicit: bool) -> Vec<Command> {
        let Some(parent) = self.surface.as_ref() else { return Vec::new() };
        let Some(placed) = parent.placed else { return Vec::new() };
        let source = parent.source;
        if source == RootSource::SelectedText {
            presentation.sentence = parent.presentation.sentence.clone();
            presentation.surface = None;
        }
        let same_word = parent.session == session && presentation.top.as_ref().zip(parent.presentation.top.as_ref())
            .is_some_and(|(a, b)| a.written == b.written && a.reading == b.reading);
        if presentation.top.is_none() || (!explicit && same_word) {
            return Vec::new();
        }
        let anchor = PhysRect {
            x: placed.popup.x.saturating_add(local.x),
            y: placed.popup.y.saturating_add(local.y), w: 1, h: 1,
        };
        let mut parent = self.surface.take().expect("parent exists");
        parent.pressed_link = None;
        parent.last_drag_point = None;
        let parents = std::mem::take(&mut self.parents);
        let pending_sentences = std::mem::take(&mut self.pending_sentences);
        let mut out = self.ready(
            presentation,
            anchor,
            Orientation::Horizontal,
            None,
            Vec::new(),
            RootTransition { source, session, replace_chain: false },
        );
        self.pending_sentences = pending_sentences;
        self.parents = parents;
        self.pointer_depth = Some(self.parents.len());
        self.parents.push(parent);
        out.retain(|cmd| !matches!(cmd, Command::ClearPopupParents));
        for cmd in &mut out {
            if let Command::ShowPopup { show_back, .. } = cmd { *show_back = true; }
        }
        out.insert(0, Command::PushPopup);
        out
    }

    /// The current popup session does not need placement geometry.
    pub fn current_session(&self) -> Option<&ProfileSession> {
        self.surface.as_ref().map(|surface| &surface.session)
    }

    /// The Anki state before and after popup placement.
    ///
    /// [`Controller::popup`] returns a value only after the platform knows the
    /// rectangle. A platform bin that paints Anki inside the popup needs this
    /// state for the first frame.
    pub fn anki(&self) -> Option<&AnkiPopupState> {
        self.surface.as_ref().map(|s| &s.anki)
    }

    /// The selection state for the shown surface, before or after placement.
    pub fn selection(&self) -> Option<&Selections> {
        self.surface.as_ref().map(|s| &s.selection)
    }

    /// A popup exists whether placement is complete or not.
    pub fn is_shown(&self) -> bool {
        self.surface.is_some()
    }

    /// Handle one `Event` through the Controller.
    pub fn handle(&mut self, event: Event) -> Vec<Command> {
        match event {
            Event::DismissRequested => {
                self.next_lookup_request();
                let mut out = self.hide();
                self.last_accepted = None;
                self.last_dispatch = None;
                self.armed_ticks = 0;
                out.extend([
                    Command::SetDragging(false), Command::SetScrollArmed(false),
                    Command::SetClickArmed(false), Command::SetAddArmed(false), Command::DiscardScroll,
                ]);
                out
            }
            Event::PopupHover { local, query } => self.popup_hover(local, query),
            Event::PopupHoverAt { depth, local, query } => self.popup_hover_at(depth, local, query),
            Event::PopupLookupRequested { depth, local, query, session } => {
                self.popup_lookup(depth, local, query, session)
            }
            Event::PopupLookupCancelled => {
                self.cancel_hover();
                if self.popup_request.is_some() { self.next_lookup_request(); }
                if let Some(surface) = self.surface.as_mut() { surface.hovered = None; }
                Vec::new()
            }
            Event::PopupActivated { depth } => {
                if self.hover_buttons != 0 { Vec::new() } else { self.enter_parent(depth) }
            }
            Event::PopupEntered { depth } => self.popup_entered(depth),
            Event::Tick { cursor, button_h } => self.tick(cursor, button_h),
            Event::GestureTick => self.gesture_tick(),
            Event::Scrolled { notches } => self.scrolled(notches),
            Event::PointerDown { local, button, hit, text } => {
                self.pointer_down(local, button, hit, text)
            }
            Event::PointerMoved { local, text } => self.pointer_moved(local, text),
            Event::PointerUp { local, button } => self.pointer_up(local, button),
            Event::AddRequested => self.add_requested(),
            Event::BackRequested => self.pop_history(),
            Event::LookupBindDown { bind_id, mode, session, pos } => {
                self.lookup_bind_down(bind_id, mode, session, pos)
            }
            Event::LookupBindUp { bind_id } => self.lookup_bind_up(&bind_id),
            Event::SelectedTextRequested { pos, session } => self.selected_text_requested(pos, session),
            Event::SelectedTextReady { id, text, bounds } => self.selected_text_ready(id, text, bounds),
            Event::PointerDownOutside => self.pointer_down_outside(),
            Event::CursorMoved { pos } => self.cursor_moved(pos),
            Event::DwellElapsed => self.dwell(),
            Event::LookupResult { id, outcome } => self.lookup_result(id, outcome),
            Event::PopupPlaced { rect, content_h, view_h } => {
                self.popup_placed(rect, content_h, view_h)
            }
            Event::PopupPlaceFailed => self.place_failed(),
            Event::DupesChecked { generation, session, dupes } => self.dupes_checked(generation, &session, dupes),
            Event::AnalysisReady { generation, words } => self.analysis_ready(generation, words),
            Event::NoteAdded { id, expr, session, failed } => self.note_added(id, expr, &session, failed),
            Event::NoteWritten { id, expr, session, status } => self.note_written(id, expr, &session, status),
            Event::ConfigReloaded { cfg, session } => self.config_reloaded(*cfg, session),
            Event::SharedResourcesReloaded { cfg, session } => self.shared_resources_reloaded(*cfg, session),
            Event::TrayAction(TrayAction::OpenSettings) => vec![Command::OpenSettings],
            Event::TrayAction(TrayAction::Quit) | Event::Quit => vec![Command::Exit],
        }
    }

    /// Create a unique request ID.
    ///
    /// `next_lookup_request` also advances the non-sentence staleness marker.
    /// Add-time sentence reads use this method directly for a unique ID.
    fn next_request(&mut self) -> RequestId {
        self.next_id += 1;
        self.latest = RequestId(self.next_id);
        self.latest
    }

    /// Create a request that replaces older non-sentence outcomes.
    fn next_lookup_request(&mut self) -> RequestId {
        let id = self.next_request();
        self.latest_lookup = id;
        self.selected_text = None;
        self.popup_request = None;
        self.request_session = None;
        id
    }
    fn request_session_default(&self) -> ProfileSession {
        let root_session = self.parents.first().or(self.surface.as_ref()).map(|surface| {
            surface.history.first().map_or(&surface.session, |entry| &entry.session)
        });
        self.active_bind.as_ref().map(|bind| &bind.session)
            .or_else(|| self.chain_bind.as_ref().map(|bind| &bind.session))
            .or(root_session)
            .unwrap_or(&self.future_session)
            .clone()
    }

    fn config_for_session(&self, session: &ProfileSession) -> ControllerConfig {
        if &self.cfg_session == session {
            return self.cfg.clone();
        }
        let mut config = ControllerConfig::for_profile(session.config());
        config.tick_ms = self.future_cfg.tick_ms;
        config
    }

    fn surface_config(&self) -> Option<&crate::config::ResolvedConfig> {
        self.surface.as_ref().map(|surface| surface.session.config())
    }

    fn set_profile_config(&mut self, session: &ProfileSession) {
        if self.cfg_session == *session {
            return;
        }
        if self.future_session == *session {
            self.cfg = self.future_cfg.clone();
            self.cfg_session = session.clone();
            return;
        }
        let mut cfg = ControllerConfig::for_profile(session.config());
        cfg.tick_ms = self.future_cfg.tick_ms;
        self.cfg = cfg;
        self.cfg_session = session.clone();
    }

    fn lookup_bind_down(
        &mut self,
        bind_id: String,
        mode: TriggerMode,
        session: ProfileSession,
        pos: PhysPoint,
    ) -> Vec<Command> {
        if let Some(active) = &self.active_bind {
            if active.id == bind_id && active.mode == mode {
                if mode == TriggerMode::Toggle {
                    return self.release_active_bind();
                }
                if matches!(mode, TriggerMode::HoldKey | TriggerMode::HoldShift) {
                    return Vec::new();
                }
            }
        }
        let mut out = Vec::new();
        if self.active_bind.as_ref().is_some_and(|active| {
            matches!(active.mode, TriggerMode::HoldKey | TriggerMode::HoldShift | TriggerMode::Toggle)
        }) {
            out.extend(self.release_active_bind());
        }
        let bind = ActiveBind { id: bind_id, mode, session: session.clone() };
        self.chain_bind = Some(bind.clone());
        self.active_bind = Some(bind);
        self.set_profile_config(&session);
        self.trigger_held = matches!(mode, TriggerMode::HoldKey | TriggerMode::HoldShift | TriggerMode::Toggle);
        self.last_accepted = Some(pos);
        let command = if matches!(mode, TriggerMode::Press | TriggerMode::HoldKey | TriggerMode::HoldShift) {
            self.trigger_pressed(pos, session)
        } else {
            self.dispatch_lookup_for(pos, session, true)
        };
        out.extend(command);
        out
    }

    fn lookup_bind_up(&mut self, bind_id: &str) -> Vec<Command> {
        let Some(active) = self.active_bind.as_ref() else { return Vec::new() };
        if active.id != bind_id || active.mode == TriggerMode::Toggle {
            return Vec::new();
        }
        self.release_active_bind()
    }

    fn release_active_bind(&mut self) -> Vec<Command> {
        let Some(active) = self.active_bind.as_ref() else { return Vec::new() };
        let mode = active.mode;
        let out = if matches!(mode, TriggerMode::HoldKey | TriggerMode::HoldShift | TriggerMode::Toggle) {
            self.trigger_up()
        } else {
            Vec::new()
        };
        self.active_bind = None;
        let session = self.request_session_default();
        self.set_profile_config(&session);
        if matches!(mode, TriggerMode::Live | TriggerMode::Press) {
            self.trigger_held = false;
        }
        out
    }

    fn config_reloaded(&mut self, cfg: ControllerConfig, session: ProfileSession) -> Vec<Command> {
        self.future_cfg = cfg;
        self.future_session = session;
        if self.surface.is_none() && self.active_bind.is_none() && self.chain_bind.is_none() {
            self.cfg = self.future_cfg.clone();
            self.cfg_session = self.future_session.clone();
        }
        let id = self.next_request();
        vec![Command::RequestReload { id }]
    }

    fn shared_resources_reloaded(
        &mut self,
        cfg: ControllerConfig,
        session: ProfileSession,
    ) -> Vec<Command> {
        self.future_cfg = cfg;
        self.future_session = session;
        self.active_bind = None;
        self.trigger_held = false;
        self.cfg = self.future_cfg.clone();
        self.cfg_session = self.future_session.clone();
        self.cancel_hover();
        let id = self.next_lookup_request();
        let mut out = self.hide();
        out.push(Command::RequestReload { id });
        out
    }
    fn tick(&mut self, cursor: PhysPoint, button_h: i32) -> Vec<Command> {
        self.clock = self.clock.wrapping_add(1);
        let tick = self.clock;
        self.button_h = button_h;
        let placed = self.surface.as_ref().and_then(|s| s.placed);
        let depth = self.popup_at(cursor);
        let hovered = depth.and_then(|depth| {
            if depth == self.parents.len() { self.surface.as_ref() } else { self.parents.get(depth) }
        }).and_then(|surface| surface.placed);
        let over_popup = hovered.is_some();
        let over_popup_or_btn = over_popup || placed.is_some_and(|p| {
            PhysRect { h: p.popup.h + button_h, ..p.popup }.contains(cursor)
        });
        let scroll_enabled = depth.is_some_and(|depth| {
            let surface = if depth == self.parents.len() {
                self.surface.as_ref()
            } else {
                self.parents.get(depth)
            };
            surface.is_some_and(|surface| surface.session.config().popup.scroll_popup)
        });
        let add_enabled = self.surface_config().is_some_and(|config| config.anki.enabled);
        let armed = scroll_enabled
            && over_popup
            && hovered.is_some_and(|p| p.content_h > p.view_h);

        let mut out = vec![
            Command::SetScrollArmed(armed),
            Command::SetClickArmed(over_popup_or_btn),
            Command::SetAddArmed(add_enabled),
        ];
        if let (Some(p), Some(s)) = (placed, self.surface.as_ref()) {
            if depth == Some(self.parents.len()) {
                out.push(Command::SetCursorShape {
                    local: PhysPoint { x: cursor.x - p.popup.x, y: cursor.y - p.popup.y },
                    scroll: s.scroll,
                });
            }
        }

        self.armed_ticks = if armed { self.armed_ticks + 1 } else { 0 };
        if self.armed_ticks == ARM_WARN_TICKS {
            out.push(Command::WarnScrollCaptured {
                seconds: (ARM_WARN_TICKS * self.cfg.tick_ms) / 1000,
            });
        }
        out.push(Command::SetBackArmed(self.has_history()));
        out.extend(self.run_gesture(GestureInput::Tick { tick }));
        out.extend(self.edge_autoscroll());
        out.extend(self.hover_tick());
        out
    }

    fn gesture_tick(&mut self) -> Vec<Command> {
        self.clock = self.clock.wrapping_add(1);
        let mut out = self.run_gesture(GestureInput::Tick { tick: self.clock });
        out.extend(self.hover_tick());
        out
    }

    fn has_history(&self) -> bool {
        !self.parents.is_empty() || self.surface.as_ref().is_some_and(|s| !s.history.is_empty())
    }

    fn repaint(&self, scroll: i32) -> Command {
        Command::RepaintPopup { scroll, show_back: self.has_history() }
    }

    fn scrolled(&mut self, notches: i32) -> Vec<Command> {
        self.cancel_hover();
        if notches == 0 {
            return Vec::new();
        }
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        let span = (p.content_h - p.view_h).max(0);
        // A positive value means wheel up.
        let step = notches.saturating_mul(SCROLL_STEP_PX);
        let next = s.scroll.saturating_sub(step).clamp(0, span);
        if next == s.scroll {
            return Vec::new();
        }
        s.scroll = next;
        vec![self.repaint(next)]
    }

    fn pointer_down(
        &mut self,
        local: PhysPoint,
        button: Button,
        hit: Option<HitAction>,
        text: Option<TextAddr>,
    ) -> Vec<Command> {
        self.cancel_hover();
        self.hover_buttons |= match button { Button::Primary => 1, Button::Secondary => 2 };
        let Some(s) = self.surface.as_ref() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        if s.scroll_drag.is_some() {
            return Vec::new();
        }
        let anki = s.session.config().anki.enabled;

        match hit {
            Some(HitAction::ScrollThumb { travel }) => {
                if button != Button::Primary || travel <= 0
                    || p.content_h <= p.view_h || !s.session.config().popup.scroll_popup {
                    return Vec::new();
                }
                let s = self.surface.as_mut().expect("checked above");
                s.scroll_drag = Some(ScrollDrag { start_y: local.y, start_scroll: s.scroll, travel });
                s.gesture.reset();
                s.pressed_link = None;
                s.last_drag_point = None;
                vec![Command::SetDragging(true)]
            }
            Some(HitAction::ExpandEntry(index)) => self.expand_entry(index),
            Some(HitAction::Back) => self.pop_history(),
            Some(HitAction::ToggleEntry(entry)) => self.toggle_entry(entry),
            Some(HitAction::DrillDown(query)) if text.is_none() || !anki => {
                let session = s.session.nested();
                let id = self.next_lookup_request();
                self.request_session = Some(LookupRequest {
                    id, session: session.clone(), replace_chain: false,
                });
                vec![Command::RequestDrillDown { id, text: query, session }]
            }
            Some(HitAction::OpenUrl(url)) if !anki => vec![Command::OpenUrl(url)],
            None if text.is_none() && local.y >= p.popup.h && anki => self.start_add(),
            _ if !anki => Vec::new(),
            hit => {
                let link = matches!(hit, Some(HitAction::OpenUrl(_)) | Some(HitAction::DrillDown(_)));
                let s = self.surface.as_mut().expect("checked above");
                s.pressed_link = link.then_some(hit).flatten();
                if text.is_some() || link {
                    if let Some(s) = self.surface.as_mut() {
                        s.last_drag_point = Some(local);
                    }
                    self.run_gesture(GestureInput::Press(PressInput {
                        addr: text,
                        link,
                        button,
                        local,
                        tick: self.clock,
                    }))
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn pointer_moved(&mut self, local: PhysPoint, text: Option<TextAddr>) -> Vec<Command> {
        if let Some(s) = self.surface.as_mut() {
            if let (Some(drag), Some(p)) = (s.scroll_drag, s.placed) {
                let span = (p.content_h - p.view_h).max(0);
                let delta = i64::from(local.y) - i64::from(drag.start_y);
                let next = (i64::from(drag.start_scroll)
                    + delta * i64::from(span) / i64::from(drag.travel))
                    .clamp(0, i64::from(span)) as i32;
                if next == s.scroll {
                    return Vec::new();
                }
                s.scroll = next;
                return vec![self.repaint(next)];
            }
        }
        if !self.surface_config().is_some_and(|config| config.anki.enabled) {
            return Vec::new();
        }
        if let Some(s) = self.surface.as_mut() {
            if s.placed.is_none() {
                return Vec::new();
            }
            s.last_drag_point = Some(local);
        } else {
            return Vec::new();
        }
        self.run_gesture(GestureInput::Move { addr: text, local, tick: self.clock })
    }

    fn pointer_up(&mut self, local: PhysPoint, button: Button) -> Vec<Command> {
        self.hover_buttons &= !match button { Button::Primary => 1, Button::Secondary => 2 };
        if self.surface.as_ref().is_some_and(|s| s.scroll_drag.is_some()) {
            if button != Button::Primary {
                return Vec::new();
            }
            let mut out = self.pointer_moved(local, None);
            self.surface.as_mut().expect("checked above").scroll_drag = None;
            out.push(Command::SetDragging(false));
            return out;
        }
        if !self.surface_config().is_some_and(|config| config.anki.enabled) {
            return Vec::new();
        }
        if self.surface.as_ref().is_none_or(|s| s.placed.is_none()) {
            return Vec::new();
        }
        let out = self.run_gesture(GestureInput::Release { button, local, tick: self.clock });
        if let Some(s) = self.surface.as_mut() {
            s.last_drag_point = None;
        }
        out
    }

    fn expand_entry(&mut self, index: usize) -> Vec<Command> {
        let Some(summary) = self.surface_config().map(|config| config.popup.summary_chars) else {
            return Vec::new();
        };
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        let card_index = index.saturating_add(1);
        if card_index >= s.presentation.all_cards.len() {
            return Vec::new();
        }
        present::swap_top(&mut s.presentation, index, summary);
        s.selection.card_mut(card_index);
        s.selection.swap(0, card_index);
        s.analysis_stale = true;
        s.gesture.reset();
        s.pressed_link = None;
        s.last_drag_point = None;
        s.scroll = 0;
        self.begin_place(PlaceKind::Reshow)
    }

    fn toggle_entry(&mut self, entry: u32) -> Vec<Command> {
        let Some(roles) = self.surface_config().map(|config| config.popup.render_settings().roles) else {
            return Vec::new();
        };
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        let Some(card) = s.presentation.top.as_ref() else { return Vec::new() };
        let Some(gloss) = entries(card).find_map(|(ordinal, value)| (ordinal == entry).then_some(value)) else {
            return Vec::new();
        };
        let Some(extent) = extent(&gloss.doc, roles) else { return Vec::new() };
        let selection = s.selection.card_mut(0);
        let on = selection.coverage(entry, extent) != Coverage::All;
        selection.set_entry(entry, extent, on);
        s.gesture.reset();
        s.pressed_link = None;
        let scroll = s.scroll;
        vec![self.repaint(scroll)]
    }

    fn run_gesture(&mut self, input: GestureInput) -> Vec<Command> {
        let Some(profile) = self.surface_config() else { return Vec::new() };
        let tick_ms = u64::from(self.future_cfg.tick_ms.max(1));
        let env = GestureEnv {
            chain_ticks: CLICK_CHAIN_MS.div_ceil(tick_ms),
            threshold_px: ANCHOR_JITTER_PX,
            primary_additive: profile.anki.selection_buttons
                == crate::config::SelectionButtons::PrimaryAdditive,
        };
        let roles = profile.popup.render_settings().roles;
        let triple_click = profile.anki.triple_click;
        let effects = {
            let Some(s) = self.surface.as_mut() else { return Vec::new() };
            let Some(card) = s.presentation.top.as_ref() else { return Vec::new() };
            let words = s.analysis.as_ref().map(|(_, words)| words);
            let source = ItemSource::new(card, roles, triple_click, words);
            let selection = s.selection.card_mut(0);
            s.gesture.handle(input, env, &source, selection)
        };
        self.gesture_commands(effects)
    }

    fn gesture_commands(&mut self, effects: Vec<GestureEffect>) -> Vec<Command> {
        let mut out = Vec::new();
        for effect in effects {
            match effect {
                GestureEffect::Repaint => {
                    if let Some(s) = self.surface.as_ref() {
                        let scroll = s.scroll;
                        out.push(self.repaint(scroll));
                    }
                }
                GestureEffect::OpenLink => {
                    let action = self.surface.as_mut().and_then(|s| s.pressed_link.take());
                    match action {
                        Some(HitAction::OpenUrl(url)) => out.push(Command::OpenUrl(url)),
                        Some(HitAction::DrillDown(text)) => {
                            let session = self.surface.as_ref()
                                .map(|surface| surface.session.nested());
                            if let Some(session) = session {
                                let id = self.next_lookup_request();
                                self.request_session = Some(LookupRequest {
                                    id, session: session.clone(), replace_chain: false,
                                });
                                out.push(Command::RequestDrillDown { id, text, session });
                            }
                        }
                        _ => {}
                    }
                }
                GestureEffect::DragStarted => out.push(Command::SetDragging(true)),
                GestureEffect::DragEnded => out.push(Command::SetDragging(false)),
                GestureEffect::NeedWord { .. } => {}
            }
        }
        out
    }

    fn edge_autoscroll(&mut self) -> Vec<Command> {
        if !self.surface_config().is_some_and(|config| {
            config.anki.enabled && config.popup.edge_autoscroll && config.popup.scroll_popup
        }) {
            return Vec::new();
        }
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        if !s.gesture.dragging() {
            return Vec::new();
        }
        let Some(local) = s.last_drag_point else { return Vec::new() };
        let (direction, overshoot) = if local.y < 0 {
            (-1, -local.y)
        } else if local.y > p.view_h {
            (1, local.y - p.view_h)
        } else {
            return Vec::new();
        };
        let step = (overshoot / 4).clamp(1, SCROLL_STEP_PX);
        let span = (p.content_h - p.view_h).max(0);
        let next = s.scroll.saturating_add(direction * step).clamp(0, span);
        if next == s.scroll {
            return Vec::new();
        }
        s.scroll = next;
        vec![self.repaint(next)]
    }

    fn add_requested(&mut self) -> Vec<Command> {
        self.cancel_hover();
        if self.popup().is_none()
            || !self.surface_config().is_some_and(|config| config.anki.enabled)
        {
            return Vec::new();
        }
        self.start_add()
    }

    fn add_in_flight(&self) -> bool {
        fn surface_add_in_flight(surface: &Surface) -> bool {
            surface.anki.adding
                || surface.pending_sentence.is_some()
                || surface.history.iter().any(|entry| {
                    entry.anki.adding || entry.pending_sentence.is_some()
                })
        }
        self.parents.iter().any(surface_add_in_flight)
            || self.surface.as_ref().is_some_and(surface_add_in_flight)
    }

    /// Apply the same guard as the pointer path.
    fn start_add(&mut self) -> Vec<Command> {
        let Some((sentence_probe, overwrite_duplicates)) = self.surface.as_ref().map(|surface| {
            let config = surface.session.config();
            (
                config.anki.sentence_mode == crate::config::SentenceMode::Sentence,
                config.anki.overwrite_duplicates,
            )
        }) else {
            return Vec::new();
        };
        if self.add_in_flight() {
            return Vec::new();
        }
        {
            let Some(s) = self.surface.as_ref() else { return Vec::new() };
            // A blocked dupe does nothing.
            if s.anki.blocks_add(present::top_expr(&s.presentation)) {
                return Vec::new();
            }
        }
        let has_hover_parent = !self.parents.is_empty();
        let sentence_probe = sentence_probe && !self.selected_text_active() && !has_hover_parent;
        let (scroll, show_back, anchor, orientation, matched_surface, session) = {
            let Some(s) = self.surface.as_ref() else { return Vec::new() };
            if s.placed.is_none() || s.presentation.top.is_none() {
                return Vec::new();
            }
            let matched_surface = if sentence_probe {
                s.presentation.surface.as_ref().and_then(OcrSurface::as_str).map(str::to_owned)
            } else {
                None
            };
            (s.scroll, !s.history.is_empty(), s.anchor, s.orientation, matched_surface, s.session.clone())
        };

        let Some((expr, fields)) = self.add_note_payload() else { return Vec::new() };
        if sentence_probe {
            let id = self.next_request();
            let hide_popup = !(self.trigger_held
                && matches!(self.trigger_mode(), TriggerMode::HoldKey | TriggerMode::HoldShift));
            let pending = PendingSentence {
                expr,
                fields,
                surface: matched_surface,
                session: session.clone(),
            };
            let s = self.surface.as_mut().expect("surface checked above");
            s.anki.adding = true;
            s.anki.saving = overwrite_duplicates;
            s.anki.failed = false;
            s.pending_sentence = Some(id);
            self.pending_sentences.insert(id, pending);
            return vec![
                Command::RepaintPopup { scroll, show_back },
                Command::RequestSentence { id, anchor, orientation, hide_popup, session },
                Command::SyncAnkiButton,
            ];
        }

        let id = self.next_request();
        let s = self.surface.as_mut().expect("surface checked above");
        s.anki.adding = true;
        s.anki.saving = overwrite_duplicates;
        s.anki.failed = false;
        s.anki_write = Some(id);
        self.pending_writes.insert(id, session.clone());
        vec![
            Command::RepaintPopup { scroll, show_back },
            Command::AddNote { id, expr, fields, session },
            Command::SyncAnkiButton,
        ]
    }

    /// Build one add payload from the current popup state.
    ///
    /// A sentence probe caller stores this result before any popup mutation.
    fn add_note_payload(&self) -> Option<(String, HashMap<String, String>)> {
        let s = self.surface.as_ref()?;
        let config = s.session.config();
        let first_dict_only = config.anki.first_dict_only;
        let include_dictionary_name = config.anki.include_dictionary_name;
        let separator = config.anki.selection_separator.into();
        s.presentation.top.as_ref()?;
        let selection = s.selection.card(0).cloned().unwrap_or_default();
        let (expr, fields) = note_payload(
            &s.presentation,
            first_dict_only,
            include_dictionary_name,
            &selection,
            separator,
        );
        if s.anki.added.contains(&expr) || s.anki.updated.contains(&expr) {
            return None;
        }
        Some((expr, fields))
    }

    fn pop_history(&mut self) -> Vec<Command> {
        self.cancel_hover();
        self.next_lookup_request();
        if self.surface.as_ref().is_some_and(|s| s.history.is_empty()) && !self.parents.is_empty() {
            return self.enter_parent(self.parents.len() - 1);
        }
        let (session, show_back) = {
            let Some(s) = self.surface.as_mut() else { return Vec::new() };
            if s.placed.is_none() {
                return Vec::new();
            }
            let Some(entry) = s.history.pop() else { return Vec::new() };
            entry.restore(s);
            if s.analysis.is_none() {
                s.analysis_stale = true;
            }
            (s.session.clone(), !s.history.is_empty())
        };
        self.set_profile_config(&session);
        let mut out = self.begin_place(PlaceKind::Reshow);
        out.push(Command::SetBackArmed(show_back));
        out
    }

    fn trigger_up(&mut self) -> Vec<Command> {
        let was_trigger_held = self.trigger_held;
        self.trigger_held = false;
        if self.selected_text_active() && !was_trigger_held {
            return Vec::new();
        }
        if self.trigger_mode() != TriggerMode::Toggle {
            return Vec::new();
        }
        self.chain_bind = None;
        self.last_accepted = None;
        self.cancel_hover();
        self.parents.clear();
        self.pending_cursor = None;
        self.next_lookup_request();
        if self.surface.take().is_none() {
            return Vec::new();
        }
        self.awaiting = None;
        self.pending_scan.clear();
        vec![Command::HidePopup, Command::SetBackArmed(false)]
    }

    /// One press asks for one lookup at `pos`. The popup then stays until a
    /// later lookup misses or the user clicks outside it. The press passes
    /// the movement gate and the sticky region because the user asked.
    fn trigger_pressed(&mut self, pos: PhysPoint, session: ProfileSession) -> Vec<Command> {
        self.last_accepted = Some(pos);
        if self.surface.as_ref().is_some_and(|surface| surface.placed.is_none()) {
            self.pending_cursor = Some((pos, session, true));
            return Vec::new();
        }
        self.dispatch_lookup_for(pos, session, true)
    }

    fn selected_text_requested(&mut self, pos: PhysPoint, session: ProfileSession) -> Vec<Command> {
        self.cancel_hover();
        self.trigger_held = false;
        let id = self.next_lookup_request();
        self.request_session = Some(LookupRequest {
            id, session: session.clone(), replace_chain: true,
        });
        self.selected_text = Some(SelectedTextState::Capturing { id, pos, session: session.clone() });
        vec![Command::ReadSelectedText { id, session }]
    }

    fn selected_text_ready(
        &mut self,
        id: RequestId,
        text: Option<String>,
        bounds: Option<PhysRect>,
    ) -> Vec<Command> {
        let (pos, session) = match self.selected_text.as_ref() {
            Some(SelectedTextState::Capturing { id: pending, pos, session })
                if *pending == id && id == self.latest_lookup => (*pos, session.clone()),
            _ => return Vec::new(),
        };
        let Some(text) = text else { return self.selected_text_unavailable() };
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_SELECTED_TEXT_BYTES {
            return self.selected_text_unavailable();
        }
        let text = text.to_string();
        let sentence_search = if self.cfg_session == session {
            self.cfg.selected_text_sentence_search
        } else {
            ControllerConfig::for_profile(session.config()).selected_text_sentence_search
        };
        if sentence_search {
            self.next_lookup_request();
            let mut commands = self.hide();
            commands.push(Command::OpenSentenceSearch { text, session });
            return commands;
        }
        let anchor = bounds.filter(|rect| rect.w > 0 && rect.h > 0)
            .unwrap_or(PhysRect { x: pos.x, y: pos.y.saturating_sub(24), w: 1, h: 48 });
        self.selected_text = Some(SelectedTextState::LookingUp {
            id,
            anchor,
            text: text.clone(),
            session: session.clone(),
        });
        vec![Command::RequestDrillDown { id, text, session }]
    }

    fn selected_text_unavailable(&mut self) -> Vec<Command> {
        self.selected_text = None;
        if self.surface.as_ref().is_some_and(|surface| {
            surface.source == RootSource::SelectedText
        }) {
            self.hide()
        } else {
            Vec::new()
        }
    }

    /// A click outside the popup dismisses it. A lookup still in flight
    /// must not show it again, so the request id moves on first.
    fn pointer_down_outside(&mut self) -> Vec<Command> {
        if self.surface.is_none() && self.selected_text.is_none() {
            return Vec::new();
        }
        self.next_lookup_request();
        self.hide()
    }

    /// Whether the current mode accepts a cursor move.
    /// Press mode never follows the cursor. Each press asks once.
    fn mode_eligible(&self) -> bool {
        if self.selected_text_active() && !self.trigger_held {
            return false;
        }
        match self.trigger_mode() {
            TriggerMode::Live => true,
            TriggerMode::Press => false,
            _ => self.trigger_held,
        }
    }

    fn selected_text_active(&self) -> bool {
        self.selected_text.is_some()
            || self.surface.as_ref().is_some_and(|surface| {
                surface.source == RootSource::SelectedText
            })
    }

    fn gate_open(&self, p: PhysPoint) -> bool {
        match self.last_accepted {
            None => true,
            Some(last) => {
                (i64::from(p.x) - i64::from(last.x)).abs() > MOVEMENT_GATE_PX
                    || (i64::from(p.y) - i64::from(last.y)).abs() > MOVEMENT_GATE_PX
            }
        }
    }

    fn cursor_moved(&mut self, pos: PhysPoint) -> Vec<Command> {
        if self.surface.as_ref().is_some_and(|s| s.scroll_drag.is_some()) {
            return Vec::new();
        }
        if self.popup_at(pos).is_some() {
            return Vec::new();
        }
        if !self.mode_eligible() || !self.gate_open(pos) {
            return Vec::new();
        }
        self.last_accepted = Some(pos);
        // Wait for the popup rectangle before deciding.
        if self.surface.as_ref().is_some_and(|s| s.placed.is_none()) {
            self.pending_cursor = Some((pos, self.request_session_default(), false));
            return Vec::new();
        }
        self.dispatch_hover(pos)
    }

    /// Keep the current result when the cursor remains in the sticky region.
    fn dispatch_hover(&mut self, pos: PhysPoint) -> Vec<Command> {
        let session = self.request_session_default();
        self.dispatch_hover_for(pos, session)
    }

    fn dispatch_hover_for(&mut self, pos: PhysPoint, session: ProfileSession) -> Vec<Command> {
        if self.frozen(pos, &session) {
            return Vec::new();
        }
        self.dispatch_lookup_for(pos, session, false)
    }


    fn dispatch_lookup_for(
        &mut self,
        pos: PhysPoint,
        session: ProfileSession,
        replace_chain: bool,
    ) -> Vec<Command> {
        self.cancel_hover();
        let popup = self.shown_popup();
        let id = self.next_lookup_request();
        self.request_session = Some(LookupRequest { id, session: session.clone(), replace_chain });
        self.last_dispatch = Some(pos);
        vec![Command::RequestLookup { id, point: pos, popup, session }]
    }
    /// Re-ask the lookup question at the point that produced the shown popup.
    ///
    /// This check bypasses the freeze gate because the cursor did not move.
    /// The screen under the cursor can change. The seams gate the new grab on
    /// damage, so unchanged pixels skip OCR and return the same presentation.
    /// `ready` then does nothing. A changed result updates the popup, and a
    /// miss hides it.
    fn dwell(&mut self) -> Vec<Command> {
        if !self.dwell_armed() {
            return Vec::new();
        }
        let Some(pos) = self.last_dispatch else { return Vec::new() };
        let popup = self.shown_popup();
        let session = self.request_session_default();
        let id = self.next_lookup_request();
        self.request_session = Some(LookupRequest { id, session: session.clone(), replace_chain: false });
        vec![Command::RequestLookup { id, point: pos, popup, session }]
    }
    /// Whether the Controller has a dwell re-check to watch. The platform bin
    /// uses this value to arm its dwell watch. No popup means no watch and no
    /// idle wakeups.
    ///
    /// Hold mode has no re-check because its frozen grab cannot change. Toggle
    /// mode reads live grabs, so the screen under a still cursor can change
    /// while the latch is on. Press mode asks only on a press.
    /// A drill-down is not screen content. A dialogue behind it must not change
    /// the history stack that the user opened.
    pub fn dwell_armed(&self) -> bool {
        if self.surface.as_ref().is_some_and(|s| s.scroll_drag.is_some()) {
            return false;
        }
        if self.selected_text_active() {
            return false;
        }
        let live = match self.trigger_mode() {
            TriggerMode::Live => true,
            TriggerMode::Toggle => self.trigger_held,
            _ => false,
        };
        live && self.parents.is_empty() && self.hover_request.is_none() && self.popup_request.is_none()
            && self.hover_candidate.is_none() && self
            .surface
            .as_ref()
            .is_some_and(|s| s.placed.is_some() && s.history.is_empty())
    }

    /// The core popup rectangle when it is on screen.
    ///
    /// Return `None` until `PopupPlaced` supplies the rectangle. An unplaced
    /// surface has no pixels, so the grab has nothing to mask.
    fn shown_popup(&self) -> Option<PhysRect> {
        Some(self.surface.as_ref()?.placed?.popup)
    }

    /// Press mode does not keep a screen lookup
    /// inside the sticky region.
    fn frozen(&self, p: PhysPoint, session: &ProfileSession) -> bool {
        if self.parents.iter().any(|s| s.placed.is_some_and(|placed| {
            in_sticky(p, s.hold, s.hold, placed.popup)
        })) {
            return true;
        }
        let mode = self.trigger_mode();
        if matches!(mode, TriggerMode::Press) {
            return false;
        }
        let Some(s) = self.surface.as_ref() else { return false };
        let Some(placed) = s.placed else { return false };
        let sticky = PhysRect { h: placed.popup.h + self.button_h, ..placed.popup };
        let freeze = if per_char_freeze(session.config().trigger.per_character_lookup, mode) {
            s.hold_char
        } else {
            s.hold
        };
        in_sticky(p, freeze, s.hold, sticky)
    }

    fn lookup_result(&mut self, id: RequestId, outcome: LookupOutcome) -> Vec<Command> {
        if let LookupOutcome::Sentence(text) = outcome {
            return self.sentence_result(id, text);
        }
        if id != self.latest_lookup {
            return Vec::new();
        }
        if self.selected_text.as_ref().is_some_and(|state| {
            matches!(state, SelectedTextState::Capturing { id: pending, .. } if *pending == id)
        }) {
            return Vec::new();
        }
        if self.selected_text.as_ref().is_some_and(|state| {
            matches!(state, SelectedTextState::LookingUp { id: pending, .. } if *pending == id)
        }) {
            if self.request_session.as_ref().is_none_or(|request| request.id != id) {
                return Vec::new();
            }
            self.request_session = None;
            return self.selected_text_lookup_result(outcome);
        }
        if self.popup_request.as_ref().is_some_and(|(request, _, _)| *request == id) {
            let (_, local, session) = self.popup_request.take().expect("matching request");
            return match outcome {
                LookupOutcome::DrillDown(presentation) => self.push_popup(*presentation, local, session, true),
                LookupOutcome::Failed(message) => vec![Command::WarnLookupFailed(message)],
                _ => Vec::new(),
            };
        }
        if self.hover_request.as_ref().is_some_and(|(request, _, _)| *request == id) {
            let (_, local, session) = self.hover_request.take().expect("matching request");
            return match outcome {
                LookupOutcome::DrillDown(presentation) => self.push_popup(*presentation, local, session, false),
                LookupOutcome::Failed(message) => vec![Command::WarnLookupFailed(message)],
                _ => Vec::new(),
            };
        }
        let Some(request) = self.request_session.take().filter(|request| request.id == id) else {
            return Vec::new();
        };
        let keep = self.keeps_popup_on_miss(&request);
        match outcome {
            LookupOutcome::Hide if keep => {
                self.last_dispatch = self.shown_dispatch;
                Vec::new()
            }
            LookupOutcome::Hide => self.hide(),
            LookupOutcome::Failed(message) if keep => {
                self.last_dispatch = self.shown_dispatch;
                vec![Command::WarnLookupFailed(message)]
            }
            LookupOutcome::Failed(message) => {
                let mut out = vec![Command::WarnLookupFailed(message)];
                out.extend(self.hide());
                out
            }
            LookupOutcome::DrillDown(presentation) => self.push_drilldown(*presentation, request.session),
            LookupOutcome::Ready { presentation, anchor, orientation, matched, scan } => {
                self.shown_dispatch = self.last_dispatch;
                self.ready(
                    *presentation,
                    anchor,
                    orientation,
                    matched,
                    scan,
                    RootTransition {
                        source: RootSource::Ocr,
                        session: request.session,
                        replace_chain: request.replace_chain,
                    },
                )
            }
            LookupOutcome::Sentence(_) => unreachable!("sentence outcomes return above"),
        }
    }

    fn selected_text_lookup_result(&mut self, outcome: LookupOutcome) -> Vec<Command> {
        let Some(SelectedTextState::LookingUp { anchor, text, session, .. }) = self.selected_text.take()
        else {
            return Vec::new();
        };
        match outcome {
            LookupOutcome::Hide => self.hide(),
            LookupOutcome::Failed(message) => {
                let mut out = vec![Command::WarnLookupFailed(message)];
                out.extend(self.hide());
                out
            }
            LookupOutcome::DrillDown(mut presentation) => {
                presentation.sentence = Some(text.clone());
                presentation.surface = presentation.top.as_ref()
                    .and_then(|top| OcrSurface::new(&text, top.match_len));
                self.ready(
                    *presentation,
                    anchor,
                    Orientation::Horizontal,
                    None,
                    Vec::new(),
                    RootTransition { source: RootSource::SelectedText, session, replace_chain: true },
                )
            }
            LookupOutcome::Ready { .. } | LookupOutcome::Sentence(_) => Vec::new(),
        }
    }

    fn sentence_result(&mut self, id: RequestId, text: Option<String>) -> Vec<Command> {
        let Some(mut pending) = self.pending_sentences.remove(&id) else { return Vec::new() };
        if let Some(sentence) = text {
            pending.fields.insert(
                "sentence".to_string(),
                bold_surface(&sentence, pending.surface.as_deref()),
            );
        }
        let mut tracked = false;
        for parent in &mut self.parents {
            tracked |= Self::mark_sentence_write(parent, id);
        }
        if let Some(surface) = self.surface.as_mut() {
            tracked |= Self::mark_sentence_write(surface, id);
        }
        self.pending_writes.insert(id, pending.session.clone());
        let mut commands = vec![Command::AddNote {
            id,
            expr: pending.expr,
            fields: pending.fields,
            session: pending.session,
        }];
        if tracked {
            commands.push(Command::SyncAnkiButton);
        }
        commands
    }

    fn mark_sentence_write(surface: &mut Surface, id: RequestId) -> bool {
        if surface.pending_sentence == Some(id) {
            surface.pending_sentence = None;
            surface.anki_write = Some(id);
            return true;
        }
        for entry in &mut surface.history {
            if entry.pending_sentence == Some(id) {
                entry.pending_sentence = None;
                entry.anki_write = Some(id);
                return true;
            }
        }
        false
    }

    fn hide(&mut self) -> Vec<Command> {
        let stop_dragging = self.surface.as_ref().is_some_and(|s| s.scroll_drag.is_some());
        self.hover_buttons = 0;
        self.cancel_hover();
        self.chain_bind = None;
        self.parents.clear();
        self.pointer_depth = None;
        self.surface = None;
        self.awaiting = None;
        self.pending_scan.clear();
        self.pending_cursor = None;
        self.selected_text = None;
        self.request_session = None;
        self.popup_request = None;
        let session = self.active_bind.as_ref().map(|bind| bind.session.clone())
            .unwrap_or_else(|| self.future_session.clone());
        self.set_profile_config(&session);
        let mut out = vec![Command::HidePopup, Command::SetBackArmed(false)];
        if stop_dragging {
            out.push(Command::SetDragging(false));
        }
        out
    }

    fn ready(
        &mut self,
        presentation: Presentation,
        anchor: PhysRect,
        orientation: Orientation,
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
        transition: RootTransition,
    ) -> Vec<Command> {
        let RootTransition { source, session, replace_chain } = transition;
        self.set_profile_config(&session);
        if !replace_chain
            && self
                .surface
                .as_ref()
                .is_some_and(|s| {
                    s.source == source
                        && s.session == session
                        && same_content(&s.presentation, s.anchor, &presentation, anchor)
                })
        {
            return Vec::new();
        }
        // A new popup invalidates probes owned by the popup it replaces.
        self.pending_sentences.clear();
        let mut out = Vec::new();
        if session.config().debug.show_lookup_log {
            if let Some(card) = &presentation.top {
                out.push(Command::LogLookup {
                    headword: card
                        .written
                        .clone()
                        .or_else(|| card.reading.clone())
                        .unwrap_or_default(),
                    match_len: card.match_len,
                });
            }
        }
        let HoldRects { hold, hold_char } = hold_regions(anchor, matched, orientation);
        let anki_enabled = session.config().anki.enabled;
        let overwrite_duplicates = session.config().anki.overwrite_duplicates;
        self.cancel_hover();
        if !self.parents.is_empty() {
            self.parents.clear();
            out.push(Command::ClearPopupParents);
        }
        if source == RootSource::SelectedText {
            self.pending_cursor = None;
        }
        self.surface = Some(Surface {
            session,
            source,
            hovered: None,
            anchor,
            orientation,
            hold,
            hold_char,
            presentation,
            anki_write: None,
            anki: AnkiPopupState::fresh(anki_enabled, overwrite_duplicates),
            history: Vec::new(),
            scroll: 0,
            scroll_drag: None,
            generation: 0,
            selection: Selections::default(),
            gesture: Gesture::default(),
            analysis: None,
            analysis_stale: true,
            analysis_generation: 0,
            pressed_link: None,
            last_drag_point: None,
            pending_sentence: None,
            placed: None,
        });
        self.pending_scan = scan;
        out.extend(self.begin_place(PlaceKind::Fresh));
        out
    }

    /// Save the current state, then replace it with the drill-down result.
    fn push_drilldown(&mut self, mut presentation: Presentation, session: ProfileSession) -> Vec<Command> {
        let Some(current) = self.surface.as_ref() else { return Vec::new() };
        if current.placed.is_none() {
            return Vec::new();
        }
        let same_headword = current.session == session
            && presentation.top.as_ref().zip(current.presentation.top.as_ref())
                .is_some_and(|(new, old)| new.written == old.written && new.reading == old.reading);
        if presentation.top.is_none() || same_headword {
            return Vec::new();
        }

        let cfg_session = session.clone();
        let cfg = self.config_for_session(&session);
        let anki_enabled = cfg.anki_enabled;
        let update_dupes = cfg.overwrite_duplicates;
        {
            let s = self.surface.as_mut().expect("current surface checked above");
            if s.source == RootSource::SelectedText {
                presentation.sentence = s.presentation.sentence.clone();
                presentation.surface = None;
            }
            let entry = HistoryEntry {
                source: s.source,
                session: std::mem::replace(&mut s.session, session),
                hovered: s.hovered.take(),
                anchor: s.anchor,
                orientation: s.orientation,
                hold: s.hold,
                hold_char: s.hold_char,
                presentation: std::mem::replace(&mut s.presentation, presentation),
                anki: std::mem::replace(&mut s.anki, AnkiPopupState::fresh(anki_enabled, update_dupes)),
                anki_write: s.anki_write.take(),
                scroll: s.scroll,
                generation: s.generation,
                selection: std::mem::take(&mut s.selection),
                gesture: std::mem::take(&mut s.gesture),
                analysis: s.analysis.take(),
                analysis_stale: s.analysis_stale,
                analysis_generation: s.analysis_generation,
                pressed_link: s.pressed_link.take(),
                last_drag_point: s.last_drag_point.take(),
                pending_sentence: s.pending_sentence.take(),
                placed: s.placed,
            };
            s.history.push(entry);
            s.hovered = None;
            s.scroll = 0;
            s.generation = 0;
            s.analysis_stale = true;
            s.analysis_generation = 0;
        }
        self.cfg = cfg;
        self.cfg_session = cfg_session;
        let mut out = self.begin_place(PlaceKind::DrillDown);
        out.push(Command::SetBackArmed(true));
        out
    }

    /// Measure, place, and show the popup again.
    fn begin_place(&mut self, kind: PlaceKind) -> Vec<Command> {
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        let stop_dragging = s.scroll_drag.take().is_some();
        if matches!(kind, PlaceKind::Fresh) {
            s.placed = None;
        }
        self.awaiting = Some(kind);
        let s = self.surface.as_ref().expect("checked above");
        let mut out = Vec::with_capacity(if stop_dragging { 2 } else { 1 });
        if stop_dragging {
            out.push(Command::SetDragging(false));
        }
        out.push(Command::ShowPopup {
            presentation: Box::new(s.presentation.clone()),
            anchor: s.anchor,
            scroll: s.scroll,
            show_back: self.has_history(),
        });
        out
    }

    fn popup_placed(&mut self, rect: PhysRect, content_h: i32, view_h: i32) -> Vec<Command> {
        let Some(kind) = self.awaiting.take() else { return Vec::new() };
        if self.surface.is_none() {
            return Vec::new();
        }
        let (session, anki_enabled, roles) = {
            let surface = self.surface.as_ref().expect("checked above");
            let config = surface.session.config();
            (
                surface.session.clone(),
                config.anki.enabled,
                config.popup.render_settings().roles,
            )
        };
        let generation = self.generation.wrapping_add(1);
        self.generation = generation;

        let mut out = Vec::new();
        let mut exprs: Vec<String> = Vec::new();
        let mut analysis_texts: Vec<(TextKey, String)> = Vec::new();
        let refresh_analysis = {
            let s = self.surface.as_mut().expect("checked above");
            s.placed = Some(Placed { popup: rect, content_h, view_h });
            let span = (content_h - view_h).max(0);
            if s.scroll > span {
                s.scroll = span;
            }
            let refresh_analysis = kind != PlaceKind::Reshow || s.analysis_stale;
            if refresh_analysis {
                s.analysis = None;
                s.analysis_generation = generation;
                s.gesture.reset();
                s.pressed_link = None;
                s.last_drag_point = None;
            }
            s.analysis_stale = false;

            match kind {
                PlaceKind::Fresh => {
                    out.push(Command::DiscardScroll);
                    out.push(Command::ShowScanOverlay {
                        rects: std::mem::take(&mut self.pending_scan),
                    });
                    out.push(Command::SyncAnkiButton);
                    if let Some(card) = &s.presentation.top {
                        if let Some(e) = card.written.as_deref().or(card.reading.as_deref()) {
                            exprs.push(e.to_string());
                        }
                    }
                    for row in &s.presentation.collapsed {
                        if let Some(e) = row.written.as_deref().or(row.reading.as_deref()) {
                            exprs.push(e.to_string());
                        }
                    }
                }
                PlaceKind::DrillDown => {
                    out.push(Command::SyncAnkiButton);
                    if let Some(card) = &s.presentation.top {
                        if let Some(e) = card.written.as_deref().or(card.reading.as_deref()) {
                            exprs.push(e.to_string());
                        }
                    }
                }
                PlaceKind::Reshow => out.push(Command::SyncAnkiButton),
            }

            if anki_enabled && refresh_analysis {
                if let Some(card) = &s.presentation.top {
                    for (entry, gloss) in entries(card) {
                        for leaf in leaves(&gloss.doc, roles) {
                            let text = leaf_text(&gloss.doc, leaf.path);
                            if !text.is_empty() {
                                analysis_texts.push(((entry, leaf.path), text.to_string()));
                            }
                        }
                    }
                }
            }
            refresh_analysis
        };

        if refresh_analysis && anki_enabled {
            if !exprs.is_empty() {
                // An analysis-only reshow must not invalidate a pending duplicate check.
                let s = self.surface.as_mut().expect("checked above");
                s.generation = generation;
                out.push(Command::CheckDupes { generation, exprs, session: session.clone() });
            }
            out.push(Command::RequestAnalysis { generation, texts: analysis_texts });
        }

        // Hold this cursor move until popup placement completes.
        if let Some((pos, session, replace_chain)) = self.pending_cursor.take() {
            if replace_chain {
                out.extend(self.dispatch_lookup_for(pos, session, true));
            } else {
                out.extend(self.dispatch_hover_for(pos, session));
            }
        }
        out
    }

    fn place_failed(&mut self) -> Vec<Command> {
        let Some(kind) = self.awaiting.take() else { return Vec::new() };
        match kind {
            PlaceKind::Fresh => {
                if !self.parents.is_empty() {
                    return self.enter_parent(self.parents.len() - 1);
                }
                self.surface = None;
                self.pending_cursor = None;
                self.pending_scan.clear();
                self.active_bind = None;
                self.chain_bind = None;
                self.trigger_held = false;
                self.last_accepted = None;
                let session = self.request_session_default();
                self.set_profile_config(&session);
                vec![
                    Command::HidePopup,
                    Command::SetScrollArmed(false),
                    Command::SetClickArmed(false),
                    Command::SetBackArmed(false),
                ]
            }
            PlaceKind::DrillDown => {
                let pending = self.pending_cursor.take();
                let Some(surface) = self.surface.as_mut() else { return Vec::new() };
                let Some(entry) = surface.history.pop() else { return Vec::new() };
                entry.restore(surface);
                let session = surface.session.clone();
                self.set_profile_config(&session);
                let mut out = vec![Command::SyncAnkiButton, Command::SetBackArmed(self.has_history())];
                if let Some((pos, session, replace_chain)) = pending {
                    if replace_chain {
                        out.extend(self.dispatch_lookup_for(pos, session, true));
                    } else {
                        out.extend(self.dispatch_hover_for(pos, session));
                    }
                }
                out
            }
            PlaceKind::Reshow => {
                match self.pending_cursor.take() {
                    Some((pos, session, true)) => self.dispatch_lookup_for(pos, session, true),
                    Some((pos, session, false)) => self.dispatch_hover_for(pos, session),
                    None => Vec::new(),
                }
            }
        }
    }
    fn analysis_ready(&mut self, generation: u64, words: WordMap) -> Vec<Command> {
        for parent in &mut self.parents {
            if parent.anki.enabled
                && parent.placed.is_some()
                && parent.analysis_generation == generation
            {
                parent.analysis = Some((generation, words));
                parent.analysis_stale = false;
                return Vec::new();
            }
            if let Some(entry) = parent.history.iter_mut().find(|entry| {
                entry.anki.enabled && entry.analysis_generation == generation && entry.placed.is_some()
            }) {
                entry.analysis = Some((generation, words));
                entry.analysis_stale = false;
                return Vec::new();
            }
        }
        let Some(surface) = self.surface.as_mut() else { return Vec::new() };
        if let Some(entry) = surface.history.iter_mut().find(|entry| {
            entry.anki.enabled && entry.analysis_generation == generation && entry.placed.is_some()
        }) {
            entry.analysis = Some((generation, words));
            entry.analysis_stale = false;
            return Vec::new();
        }
        if !surface.anki.enabled || surface.placed.is_none() || surface.analysis_generation != generation {
            return Vec::new();
        }
        surface.analysis = Some((generation, words));
        surface.analysis_stale = false;
        self.run_gesture(GestureInput::Analysis)
    }

    fn dupes_checked(
        &mut self,
        generation: u64,
        session: &ProfileSession,
        dupes: Option<HashSet<String>>,
    ) -> Vec<Command> {
        for parent in &mut self.parents {
            if parent.generation == generation && &parent.session == session {
                apply_dupe_status(&mut parent.anki, dupes);
                return Vec::new();
            }
            if let Some(entry) = parent.history.iter_mut().find(|entry| {
                entry.generation == generation && &entry.session == session
            }) {
                apply_dupe_status(&mut entry.anki, dupes);
                return Vec::new();
            }
        }
        let Some(surface) = self.surface.as_mut() else { return Vec::new() };
        if let Some(entry) = surface.history.iter_mut().find(|entry| {
            entry.generation == generation && &entry.session == session
        }) {
            apply_dupe_status(&mut entry.anki, dupes);
            return Vec::new();
        }
        if surface.generation != generation || &surface.session != session || surface.placed.is_none() {
            return Vec::new();
        }
        apply_dupe_status(&mut surface.anki, dupes);
        self.begin_place(PlaceKind::Reshow)
    }

    fn note_added(
        &mut self,
        id: RequestId,
        expr: String,
        session: &ProfileSession,
        failed: bool,
    ) -> Vec<Command> {
        self.note_written(
            id,
            expr,
            session,
            if failed { NoteWriteStatus::Failed } else { NoteWriteStatus::Added },
        )
    }

    fn note_written(
        &mut self,
        id: RequestId,
        expr: String,
        session: &ProfileSession,
        status: NoteWriteStatus,
    ) -> Vec<Command> {
        if self.pending_writes.get(&id).is_none_or(|owner| owner != session) {
            return Vec::new();
        }
        self.pending_writes.remove(&id);
        for parent in &mut self.parents {
            apply_note_write(parent, id, session, &expr, status);
        }
        let current = self.surface.as_mut()
            .is_some_and(|surface| apply_note_write(surface, id, session, &expr, status));
        if current {
            self.begin_place(PlaceKind::Reshow)
        } else {
            Vec::new()
        }
}
}


fn apply_dupe_status(anki: &mut AnkiPopupState, dupes: Option<HashSet<String>>) {
    anki.checking = false;
    match dupes {
        Some(dupes) => {
            anki.connected = true;
            anki.dupes = dupes;
        }
        None => anki.connected = false,
    }
}
fn apply_note_write(
    surface: &mut Surface,
    id: RequestId,
    session: &ProfileSession,
    expr: &str,
    status: NoteWriteStatus,
) -> bool {
    if surface.anki_write == Some(id) && surface.session == *session {
        surface.anki_write = None;
        apply_note_status(&mut surface.anki, expr, status);
        return true;
    }
    for entry in &mut surface.history {
        if entry.anki_write == Some(id) && entry.session == *session {
            entry.anki_write = None;
            apply_note_status(&mut entry.anki, expr, status);
        }
    }
    false
}

fn apply_note_status(anki: &mut AnkiPopupState, expr: &str, status: NoteWriteStatus) {
    anki.adding = false;
    match status {
        NoteWriteStatus::Failed => anki.failed = true,
        NoteWriteStatus::Added => {
            anki.failed = false;
            anki.added.insert(expr.to_string());
        }
        NoteWriteStatus::Updated => {
            anki.failed = false;
            anki.updated.insert(expr.to_string());
        }
    }
}

/// Returns true when the content matches and anchor movement stays within the jitter limit.
fn same_content(
    prev: &Presentation,
    prev_anchor: PhysRect,
    new: &Presentation,
    anchor: PhysRect,
) -> bool {
    prev == new
        && (prev_anchor.x - anchor.x).abs() <= ANCHOR_JITTER_PX
        && (prev_anchor.y - anchor.y).abs() <= ANCHOR_JITTER_PX
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::SelRange;
    use crate::present::{Card, GlossBlock};
    use std::cell::RefCell;

    thread_local! {
        static CURRENT_TEST_SESSION: RefCell<Option<ProfileSession>> = const { RefCell::new(None) };
    }

    fn session_for_controller_cfg(cfg: &ControllerConfig) -> ProfileSession {
        session_for_controller_cfg_with(cfg, |_| {})
    }

    fn session_for_controller_cfg_with(
        cfg: &ControllerConfig,
        edit: impl FnOnce(&mut crate::config::ProfileSettings),
    ) -> ProfileSession {
        assert!(matches!(cfg.trigger_mode, TriggerMode::Live | TriggerMode::Press));
        let mut config = crate::config::Config {
            live_lookup: cfg.trigger_mode == TriggerMode::Live,
            ..Default::default()
        };
        let default_profile = config.default_profile.clone();
        {
            let profile = config.profiles.iter_mut()
                .find(|profile| profile.id == default_profile)
                .expect("default profile exists");
            let crate::config::ProfileData::Full { settings } = &mut profile.data else {
                panic!("default profile has full settings");
            };
            settings.popup.sub_popups = cfg.sub_popups;
            settings.popup.scroll_popup = cfg.scroll_popup;
            settings.popup.summary_chars = cfg.summary_chars;
            settings.popup.edge_autoscroll = cfg.edge_autoscroll;
            settings.popup.show_examples = cfg.roles.examples;
            settings.popup.show_attributions = cfg.roles.attributions;
            settings.popup.show_part_of_speech = cfg.roles.part_of_speech;
            settings.per_character_lookup = cfg.per_character_lookup;
            settings.anki.enabled = cfg.anki_enabled;
            settings.anki.overwrite_duplicates = cfg.overwrite_duplicates;
            settings.anki.sentence_mode = if cfg.sentence_probe {
                crate::config::SentenceMode::Sentence
            } else {
                crate::config::SentenceMode::Line
            };
            settings.anki.include_dictionary_name = cfg.include_dictionary_name;
            settings.anki.first_dict_only = cfg.first_dict_only;
            settings.anki.selection_buttons = if cfg.primary_additive {
                crate::config::SelectionButtons::PrimaryAdditive
            } else {
                crate::config::SelectionButtons::PrimaryReplacing
            };
            settings.anki.selection_separator = match cfg.separator {
                Separator::Ellipsis => crate::config::SelectionSeparator::Ellipsis,
                Separator::Space => crate::config::SelectionSeparator::Space,
                Separator::LineBreak => crate::config::SelectionSeparator::LineBreak,
                Separator::ListItems => crate::config::SelectionSeparator::ListItems,
            };
            settings.anki.triple_click = cfg.triple_click;
            settings.actions.search.selected_opens_sentence_search =
                cfg.selected_text_sentence_search;
            edit(settings);
        }
        config.debug.show_lookup_log = cfg.log_lookups;
        crate::config::ProfileCatalog::new(&config, &[])
            .expect("test config builds")
            .session(None)
            .expect("default profile exists")
    }

    fn remember_test_session(session: &ProfileSession) {
        CURRENT_TEST_SESSION.with(|stored| *stored.borrow_mut() = Some(session.clone()));
    }

    fn test_session() -> ProfileSession {
        CURRENT_TEST_SESSION.with(|stored| {
            if let Some(session) = stored.borrow().clone() {
                return session;
            }
            let session = session_for_controller_cfg(&cfg());
            *stored.borrow_mut() = Some(session.clone());
            session
        })
    }

    fn test_controller(cfg: ControllerConfig) -> Controller {
        let session = session_for_controller_cfg(&cfg);
        remember_test_session(&session);
        Controller::new(cfg, session)
    }

    fn reload(c: &mut Controller, cfg: ControllerConfig) -> Vec<Command> {
        let session = session_for_controller_cfg(&cfg);
        remember_test_session(&session);
        c.handle(Event::ConfigReloaded { cfg: Box::new(cfg), session })
    }

    fn bind_down(c: &mut Controller, bind_id: &str, mode: TriggerMode, pos: PhysPoint) -> Vec<Command> {
        let session = c.future_session.clone();
        remember_test_session(&session);
        c.handle(Event::LookupBindDown {
            bind_id: bind_id.to_string(),
            mode,
            session,
            pos,
        })
    }

    fn bind_up(c: &mut Controller, bind_id: &str) -> Vec<Command> {
        c.handle(Event::LookupBindUp { bind_id: bind_id.to_string() })
    }

    fn hover_request(c: &mut Controller, query: &str) -> RequestId {
        let session = c.surface.as_ref().expect("shown popup").session.nested();
        remember_test_session(&session);
        c.handle(Event::PopupHover { local: PhysPoint { x: 50, y: 70 }, query: Some(query.into()) });
        let commands: Vec<_> = (0..16).flat_map(|_| c.handle(Event::GestureTick)).collect();
        commands.into_iter().find_map(|cmd| match cmd {
            Command::RequestDrillDown { id, .. } => Some(id), _ => None,
        }).expect("hover must issue dictionary lookup")
    }

    fn selected_text_request(c: &mut Controller, pos: PhysPoint) -> RequestId {
        let session = c.request_session_default();
        remember_test_session(&session);
        match c.handle(Event::SelectedTextRequested { pos, session }).as_slice() {
            [Command::ReadSelectedText { id, .. }] => *id,
            commands => panic!("unexpected selection commands: {commands:?}"),
        }
    }

    fn hover_child(c: &mut Controller, word: &str) {
        let id = hover_request(c, word);
        let commands = c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of(word))) });
        assert!(commands.contains(&Command::PushPopup));
        let depth = c.popup_depth() as i32;
        c.handle(placed(PhysRect { x: 100 + depth * 150, y: 160 + depth * 60, ..POPUP }, 700, 200));
    }

    fn popup_shortcut(c: &mut Controller, depth: usize, session: ProfileSession, text: &str) -> RequestId {
        c.handle(Event::PopupLookupRequested {
            depth, local: PhysPoint { x: 50, y: 70 }, query: Some(text.into()), session,
        }).into_iter().find_map(|command| match command {
            Command::RequestDrillDown { id, .. } => Some(id),
            Command::RequestLookup { .. } => panic!("popup shortcuts must not request OCR"),
            _ => None,
        }).expect("popup text starts a dictionary lookup")
    }

    #[test]
    fn capture_hide_cancels_popup_requests_without_dismissing_the_parent() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let session = c.popup().unwrap().session.clone();
        let id = popup_shortcut(&mut c, 0, session.clone(), "犬");
        c.handle(Event::PopupLookupCancelled);
        assert!(c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        }).is_empty());
        assert_eq!(c.popup_depth(), 0);
        assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("猫"));
        let resumed = popup_shortcut(&mut c, 0, session, "鳥");
        assert!(c.handle(Event::LookupResult {
            id: resumed, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("鳥"))),
        }).contains(&Command::PushPopup));
        c.handle(placed(POPUP, 200, 200));
        let hovered = hover_request(&mut c, "魚");
        c.handle(Event::PopupLookupCancelled);
        assert!(c.handle(Event::LookupResult {
            id: hovered, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("魚"))),
        }).is_empty());
        assert_eq!(c.popup_depth(), 1);
        let restarted = hover_request(&mut c, "魚");
        assert!(c.handle(Event::LookupResult {
            id: restarted, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("魚"))),
        }).contains(&Command::PushPopup));
    }

    #[test]
    fn stationary_hover_keeps_the_shortcut_profile_child() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let (_, _, session) = nested_test_sessions();
        let id = popup_shortcut(&mut c, 0, session.clone(), "犬");
        let local = PhysPoint { x: 50, y: 70 };
        c.handle(Event::PopupHoverAt { depth: 0, local, query: Some("犬".into()) });
        c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        c.handle(placed(POPUP, 200, 200));
        c.handle(Event::PopupHoverAt { depth: 0, local, query: Some("犬".into()) });
        for _ in 0..20 { c.handle(Event::GestureTick); }
        assert_eq!(c.popup_depth(), 1);
        assert_eq!(c.popup().unwrap().session, &session);
    }

    #[test]
    fn popup_shortcuts_cross_profiles_without_hover_or_a_depth_limit() {
        let root_cfg = ControllerConfig { sub_popups: false, ..cfg() };
        let mut c = test_controller(root_cfg);
        shown_sized(&mut c, 700, 200);
        c.handle(Event::Scrolled { notches: -2 });
        let root = c.popup().unwrap().session.clone();
        let (_, child, grandchild) = nested_test_sessions();
        for depth in 0..20 {
            let session = if depth % 2 == 0 { child.clone() } else { grandchild.clone() };
            let id = popup_shortcut(&mut c, depth, session.clone(), "犬");
            c.handle(Event::PopupHover {
                local: PhysPoint { x: 51, y: 70 }, query: None,
            });
            let commands = c.handle(Event::LookupResult {
                id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
            });
            assert!(commands.contains(&Command::PushPopup));
            c.handle(placed(POPUP, 200, 200));
            assert_eq!(c.popup().unwrap().session, &session);
            assert_eq!(c.popup_depth(), depth + 1);
        }
        assert_eq!(c.parent_popup(0).unwrap().session, &root);
        assert_eq!(c.parent_popup(0).unwrap().scroll, 96);
        let id = popup_shortcut(&mut c, 0, grandchild.clone(), "鳥");
        assert_eq!(c.popup_depth(), 0);
        c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("鳥"))),
        });
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(c.popup_depth(), 1);
        assert_eq!(c.popup().unwrap().session, &grandchild);
        c.handle(Event::BackRequested);
        assert_eq!(c.popup().unwrap().session, &root);
        assert_eq!(c.popup().unwrap().scroll, 96);
    }

    #[test]
    fn popup_shortcuts_can_repeat_the_same_word_and_profile() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let session = c.popup().unwrap().session.clone();
        let id = popup_shortcut(&mut c, 0, session, "猫");
        let commands = c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("猫"))),
        });
        assert!(commands.contains(&Command::PushPopup));
        assert_eq!(c.popup_depth(), 1);
    }

    #[test]
    fn popup_shortcut_misses_keep_parents_and_navigation_rejects_stale_results() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let session = c.popup().unwrap().session.clone();
        let id = popup_shortcut(&mut c, 0, session.clone(), "犬");
        c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        c.handle(placed(POPUP, 200, 200));
        let miss = popup_shortcut(&mut c, 1, session.clone(), "見つからない");
        c.handle(Event::LookupResult { id: miss, outcome: LookupOutcome::Hide });
        assert_eq!(c.popup_depth(), 1);
        assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("犬"));
        let old = popup_shortcut(&mut c, 1, session.clone(), "鳥");
        let new = popup_shortcut(&mut c, 1, session.clone(), "魚");
        assert!(c.handle(Event::LookupResult {
            id: old, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("鳥"))),
        }).is_empty());
        c.handle(Event::BackRequested);
        assert!(c.handle(Event::LookupResult {
            id: new, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("魚"))),
        }).is_empty());
        assert_eq!(c.popup_depth(), 0);
        assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("猫"));
        assert!(c.handle(Event::PopupLookupRequested {
            depth: 0, local: PhysPoint { x: 0, y: 0 }, query: None, session: session.clone(),
        }).is_empty());
        let dismissed = popup_shortcut(&mut c, 0, session, "犬");
        c.handle(Event::DismissRequested);
        assert!(c.handle(Event::LookupResult {
            id: dismissed, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        }).is_empty());
        assert!(!c.is_shown());
    }

    #[test]
    fn popup_shortcuts_preserve_root_trigger_ownership() {
        for mode in [TriggerMode::Press, TriggerMode::HoldKey, TriggerMode::Toggle] {
            let mut c = test_controller(press_cfg());
            bind_down(&mut c, "root", mode, PhysPoint { x: 110, y: 110 });
            shown(&mut c);
            let (_, session, _) = nested_test_sessions();
            let id = popup_shortcut(&mut c, 0, session.clone(), "犬");
            bind_up(&mut c, "child");
            assert_eq!(c.active_bind_id(), Some("root"));
            assert_eq!(c.trigger_mode(), mode);
            c.handle(Event::LookupResult {
                id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
            });
            c.handle(placed(POPUP, 200, 200));
            assert_eq!(c.popup().unwrap().session, &session);
            assert_eq!(c.popup_depth(), 1);
            assert!(bind_up(&mut c, "child").is_empty());
            assert_eq!(c.popup_depth(), 1);
        }
    }

    #[test]
    fn hover_parent_reentry_restores_the_correct_level_and_preserves_state() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 700, 200);
        c.handle(Event::Scrolled { notches: -2 });
        c.surface.as_mut().unwrap().selection.card_mut(0).replace(SelRange {
            start: TextAddr { entry: 0, addr: crate::select::DocAddr::START },
            end: TextAddr { entry: 0, addr: crate::select::DocAddr::END },
        });
        hover_child(&mut c, "犬");
        let root = c.parents[0].clone();
        c.handle(Event::Scrolled { notches: -3 });
        hover_child(&mut c, "鳥");
        let parent = c.parents[1].clone();
        c.handle(Event::PopupEntered { depth: 2 });
        let commands = c.handle(Event::PopupEntered { depth: 1 });
        assert!(commands.contains(&Command::RestorePopup { depth: 1 }));
        assert_eq!(c.surface.as_ref(), Some(&parent));
        assert_eq!(c.parents, vec![root.clone()]);
        c.handle(Event::BackRequested);
        assert_eq!(c.surface.as_ref(), Some(&root));
        assert!(c.parents.is_empty());
    }

    #[test]
    fn a_profile_reload_preserves_the_active_chain() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = hover_request(&mut c, "犬");
        let mut disabled = cfg();
        disabled.sub_popups = false;
        reload(&mut c, disabled);
        let out = c.handle(Event::LookupResult {
            id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        assert!(out.contains(&Command::PushPopup));
        assert_eq!(c.popup_depth(), 1);
        assert!(c.cfg.sub_popups, "the active chain keeps its original settings");
    }


    #[test]
    fn selected_text_uses_dictionary_lookup_and_preserves_sentence_context() {
        let mut config = cfg();
        config.anki_enabled = true;
        config.sentence_probe = true;
        let mut c = test_controller(config);
        let pos = PhysPoint { x: 320, y: 240 };
        let id = selected_text_request(&mut c, pos);
        assert_eq!(
            vec![Command::RequestDrillDown {
                id, text: "日本語の文".into(), session: test_session(),
            }],
            c.handle(Event::SelectedTextReady {
                id,
                text: Some("  日本語の文  ".into()), bounds: None })
        );
        let out = c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("日本語"))),
        });
        assert!(out.iter().any(|command| matches!(command, Command::ShowPopup { .. })));
        let surface = c.surface.as_ref().expect("selected-text popup");
        assert_eq!(RootSource::SelectedText, surface.source);
        assert_eq!(Some("日本語の文"), surface.presentation.sentence.as_deref());
        assert_eq!(
            Some("日本語"),
            surface.presentation.surface.as_ref().and_then(OcrSurface::as_str)
        );
        c.handle(placed(POPUP, 200, 200));
        hover_child(&mut c, "語");
        let child = c.surface.as_ref().expect("selected-text child");
        assert_eq!(RootSource::SelectedText, child.source);
        assert_eq!(Some("日本語の文"), child.presentation.sentence.as_deref());
        let session = c.surface.as_ref().expect("selected-text child").session.nested();
        c.push_drilldown(presentation_of("文"), session);
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(
            Some("日本語の文"),
            c.surface.as_ref().and_then(|surface| surface.presentation.sentence.as_deref())
        );
        let add = c.handle(Event::AddRequested);
        assert!(add.iter().any(|command| matches!(command, Command::AddNote { .. })));
        assert!(add.iter().all(|command| !matches!(command, Command::RequestSentence { .. })));
    }

    fn cfg() -> ControllerConfig {
        ControllerConfig {
            sub_popups: true,
            selected_text_sentence_search: false,
            sentence_probe: false,
            trigger_mode: TriggerMode::Live,
            per_character_lookup: false,
            scroll_popup: true,
            anki_enabled: false,
            overwrite_duplicates: false,
            include_dictionary_name: true,
            first_dict_only: false,
            summary_chars: 60,
            log_lookups: false,
            tick_ms: 20,
            roles: RoleFilter::default(),
            edge_autoscroll: true,
            primary_additive: true,
            separator: Separator::Ellipsis,
            triple_click: TripleClick::SenseWithExamples,
        }
    }

    fn card(written: &str) -> Card {
        Card {
            written: Some(written.to_string()),
            reading: None,
            pos: Vec::new(),
            inflections: Vec::new(),
            freq: None,
            blocks: Vec::new(),
            match_len: written.chars().count(),
            pitch: Vec::new(),
        }
    }

    fn presentation_of(written: &str) -> Presentation {
        Presentation {
            top: Some(card(written)),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: None,
            surface: None,
        }
    }

    const ANCHOR: PhysRect = PhysRect { x: 100, y: 100, w: 20, h: 20 };
    const POPUP: PhysRect = PhysRect { x: 100, y: 160, w: 300, h: 200 };

    fn ready(written: &str, anchor: PhysRect) -> Event {
        ready_id(RequestId(1), written, anchor)
    }

    /// Build the result for one request with unchanged content.
    fn ready_id(id: RequestId, written: &str, anchor: PhysRect) -> Event {
        Event::LookupResult {
            id,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation_of(written)),
                anchor,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        }
    }

    fn placed(rect: PhysRect, content_h: i32, view_h: i32) -> Event {
        Event::PopupPlaced { rect, content_h, view_h }
    }

    fn click(
        c: &mut Controller,
        local: PhysPoint,
        button: Button,
        hit: Option<HitAction>,
    ) -> Vec<Command> {
        let session = if matches!(hit.as_ref(), Some(HitAction::DrillDown(_))) {
            c.surface.as_ref().expect("shown popup").session.nested()
        } else {
            c.surface.as_ref().expect("shown popup").session.clone()
        };
        remember_test_session(&session);
        let out = c.handle(Event::PointerDown { local, button, hit, text: None });
        c.handle(Event::PointerUp { local, button });
        out
    }

    /// Show a placed popup with the default size.
    fn shown(c: &mut Controller) {
        shown_sized(c, 200, 200);
    }

    /// Show a placed popup with the given content and size.
    fn shown_sized(c: &mut Controller, content_h: i32, view_h: i32) {
        remember_test_session(&c.request_session_default());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let id = c.latest;
        c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation_of("\u{732B}")),
                anchor: ANCHOR,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        c.handle(placed(POPUP, content_h, view_h));
    }

    fn presentation_with_card(card: Card, all_cards: Vec<Card>) -> Presentation {
        Presentation {
            top: Some(card),
            collapsed: Vec::new(),
            all_cards,
            sentence: None,
            surface: None,
        }
    }

    fn shown_card(c: &mut Controller, presentation: Presentation) {
        remember_test_session(&c.request_session_default());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let id = c.latest;
        c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation),
                anchor: ANCHOR,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        c.handle(placed(POPUP, 200, 200));
    }

    /// Mark the shown headword as a known duplicate for Anki.
    fn a_known_dupe(c: &mut Controller) {
        c.handle(Event::DupesChecked {
            generation: 1,
            dupes: Some(HashSet::from(["\u{732B}".to_string()])),
            session: test_session(),
        });
        c.handle(placed(POPUP, 200, 200));
    }

    fn gloss_card(first: &str, second: &str) -> Card {
        let glossary = serde_json::json!([first, second]).to_string();
        Card {
            written: Some(first.to_string()),
            blocks: vec![GlossBlock::parse("Test", &glossary)],
            ..card(first)
        }
    }

    // -- armed controls --

    // -- movement gate --

    // -- trigger freeze --

    fn hold_cfg() -> ControllerConfig {
        press_cfg()
    }

    fn press_cfg() -> ControllerConfig {
        ControllerConfig { trigger_mode: TriggerMode::Press, ..cfg() }
    }

    /// Show a placed popup from one Press bind.
    fn pressed_and_shown(c: &mut Controller, pos: PhysPoint) {
        bind_down(c, "press", TriggerMode::Press, pos);
        let id = c.latest;
        c.handle(ready_id(id, "\u{732B}", ANCHOR));
        c.handle(placed(POPUP, 200, 200));
    }

    #[test]
    fn press_mode_looks_up_once_per_press_and_never_follows_the_cursor() {
        let mut c = test_controller(press_cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        assert!(c.handle(Event::CursorMoved { pos }).is_empty(), "no hover before a press");
        assert_eq!(
            vec![Command::RequestLookup { id: RequestId(1), point: pos, popup: None, session: test_session() }],
            bind_down(&mut c, "press", TriggerMode::Press, pos)
        );
        bind_up(&mut c, "press");
        c.handle(ready_id(RequestId(1), "\u{732B}", ANCHOR));
        c.handle(placed(POPUP, 200, 200));
        assert!(c.is_shown());
        let away = PhysPoint { x: 900, y: 900 };
        assert!(c.handle(Event::CursorMoved { pos: away }).is_empty(), "no hover after a press");
        assert!(c.handle(Event::DwellElapsed).is_empty(), "no dwell re-check");
        assert!(c.is_shown());
    }

    #[test]
    fn a_click_outside_hides_the_popup_and_kills_the_answer_in_flight() {
        let mut c = test_controller(press_cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        pressed_and_shown(&mut c, pos);
        bind_down(&mut c, "press", TriggerMode::Press, PhysPoint { x: 500, y: 500 });
        let stale = c.latest;
        let out = c.handle(Event::PointerDownOutside);
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
        bind_up(&mut c, "press");
        assert!(c.handle(ready_id(stale, "\u{72AC}", ANCHOR)).is_empty());
        assert!(!c.is_shown());
        assert!(c.handle(Event::PointerDownOutside).is_empty(), "nothing to hide twice");
    }

    fn exit_keeping_controller(cfg: ControllerConfig) -> Controller {
        let session = session_for_controller_cfg_with(&cfg, |settings| {
            settings.popup.hide_on_cursor_exit = false;
        });
        remember_test_session(&session);
        Controller::new(cfg, session)
    }

    fn cursor_miss(c: &mut Controller, pos: PhysPoint) -> Vec<Command> {
        let out = c.handle(Event::CursorMoved { pos });
        assert!(
            out.iter().any(|command| matches!(command, Command::RequestLookup { .. })),
            "the cursor left the sticky region, so it asks again",
        );
        let id = c.latest;
        c.handle(Event::LookupResult { id, outcome: LookupOutcome::Hide })
    }

    #[test]
    fn with_hide_on_cursor_exit_off_only_an_outside_click_hides_the_popup() {
        let mut c = exit_keeping_controller(cfg());
        shown(&mut c);
        assert!(c.watches_outside_clicks());
        assert!(c.swallows_outside_clicks());

        let out = cursor_miss(&mut c, PhysPoint { x: 900, y: 900 });
        assert!(!out.contains(&Command::HidePopup), "{out:?}");
        assert!(c.is_shown());
        let out = cursor_miss(&mut c, PhysPoint { x: 1500, y: 40 });
        assert!(!out.contains(&Command::HidePopup), "{out:?}");
        assert!(c.is_shown());

        c.handle(Event::CursorMoved { pos: PhysPoint { x: 40, y: 900 } });
        let id = c.latest;
        let out = c.handle(Event::LookupResult { id, outcome: LookupOutcome::Failed("ocr".into()) });
        assert_eq!(out, vec![Command::WarnLookupFailed("ocr".into())]);
        assert!(c.is_shown(), "a failed cursor lookup keeps the popup too");

        let out = c.handle(Event::PointerDownOutside);
        assert!(out.contains(&Command::HidePopup));
        assert!(!c.is_shown());
        assert!(!c.watches_outside_clicks());
        assert!(!c.swallows_outside_clicks());
    }

    #[test]
    fn a_hold_tap_shows_the_popup_and_it_stays_until_an_outside_click() {
        let mut c = test_controller(hold_cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        bind_down(&mut c, "hold", TriggerMode::HoldKey, pos);
        let id = c.latest;
        assert!(bind_up(&mut c, "hold").is_empty());
        c.handle(ready_id(id, "\u{732B}", ANCHOR));
        c.handle(placed(POPUP, 200, 200));
        assert!(c.is_shown());
        assert!(c.watches_outside_clicks());
        assert!(c.swallows_outside_clicks());
        assert!(c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } }).is_empty());
        assert!(c.handle(Event::DwellElapsed).is_empty());
        assert!(c.is_shown());
        let out = c.handle(Event::PointerDownOutside);
        assert!(out.contains(&Command::HidePopup), "{out:?}");
        assert!(!c.is_shown());
    }

    // -- the sticky region --

    // -- dwell re-check --

    /// A dwell re-check has three outcomes. Unchanged content does nothing,
    /// changed content updates the popup, and no result hides it.
    /// A static screen therefore needs no extra OCR pass.
    #[test]
    fn a_dwell_answer_presents_only_a_change() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        c.handle(Event::DwellElapsed);
        assert!(
            c.handle(ready_id(c.latest, "\u{732B}", ANCHOR)).is_empty(),
            "the same card at the same anchor is not a redraw"
        );

        c.handle(Event::DwellElapsed);
        let out = c.handle(ready_id(c.latest, "\u{98DF}\u{3079}\u{308B}", ANCHOR));
        assert!(
            out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })),
            "advancing dialogue refreshes the popup: {out:?}"
        );
        c.handle(placed(POPUP, 200, 200));

        c.handle(Event::DwellElapsed);
        let out = c.handle(Event::LookupResult { id: c.latest, outcome: LookupOutcome::Hide });
        assert!(out.contains(&Command::HidePopup), "a miss retracts it: {out:?}");
        assert!(!c.dwell_armed(), "and the watch has nothing left to do");
    }

    // -- popup placement --

    #[test]
    fn a_failed_placement_retracts_a_new_popup() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        let out = c.handle(Event::PopupPlaceFailed);
        assert_eq!(
            out,
            vec![
                Command::HidePopup,
                Command::SetScrollArmed(false),
                Command::SetClickArmed(false),
                Command::SetBackArmed(false),
            ]
        );
        assert!(!c.is_shown());
    }

    // -- scroll behavior --

    #[test]
    fn dragging_the_scroll_thumb_preserves_the_grab_and_stops_on_release() {
        for anki_enabled in [false, true] {
            let mut c = test_controller(ControllerConfig { anki_enabled, ..cfg() });
            shown_sized(&mut c, 800, 200);
            c.handle(Event::Scrolled { notches: -2 });
            let down = PhysPoint { x: 290, y: 40 };
            let commands = c.handle(Event::PointerDown {
                local: down, button: Button::Primary,
                hit: Some(HitAction::ScrollThumb { travel: 120 }), text: None,
            });
            assert!(commands.contains(&Command::SetDragging(true)));
            assert_eq!(96, c.popup().unwrap().scroll);
            assert!(!c.dwell_armed());
            assert!(c.handle(Event::CursorMoved {
                pos: PhysPoint { x: 20, y: 20 },
            }).is_empty());
            c.handle(Event::PointerUp { local: down, button: Button::Secondary });
            let moved = PhysPoint { x: -20, y: 60 };
            assert_eq!(
                vec![Command::RepaintPopup { scroll: 196, show_back: false }],
                c.handle(Event::PointerMoved { local: moved, text: None }),
            );
            c.handle(Event::PointerMoved {
                local: PhysPoint { x: -20, y: 400 }, text: None,
            });
            assert_eq!(600, c.popup().unwrap().scroll);
            c.handle(Event::PointerMoved {
                local: PhysPoint { x: -20, y: -200 }, text: None,
            });
            assert_eq!(0, c.popup().unwrap().scroll);
            c.handle(Event::PointerMoved { local: moved, text: None });
            let commands = c.handle(Event::PointerUp { local: moved, button: Button::Primary });
            assert!(commands.contains(&Command::SetDragging(false)));
            c.handle(Event::PointerMoved {
                local: PhysPoint { x: 290, y: 100 }, text: None,
            });
            assert_eq!(196, c.popup().unwrap().scroll);
        }
    }

    #[test]
    fn the_scroll_thumb_requires_primary_input_and_enabled_overflow() {
        for (button, scroll_popup, content_h, travel) in [
            (Button::Secondary, true, 800, 120),
            (Button::Primary, false, 800, 120),
            (Button::Primary, true, 200, 120),
            (Button::Primary, true, 800, 0),
        ] {
            let mut c = test_controller(ControllerConfig { scroll_popup, ..cfg() });
            shown_sized(&mut c, content_h, 200);
            let commands = c.handle(Event::PointerDown {
                local: PhysPoint { x: 290, y: 40 }, button,
                hit: Some(HitAction::ScrollThumb { travel }), text: None,
            });
            assert!(!commands.contains(&Command::SetDragging(true)));
            c.handle(Event::PointerMoved {
                local: PhysPoint { x: 290, y: 100 }, text: None,
            });
            assert_eq!(0, c.popup().unwrap().scroll);
        }
    }

    #[test]
    fn hiding_or_remeasuring_a_popup_cancels_its_scroll_thumb_drag() {
        for dismiss in [false, true] {
            let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
            shown_sized(&mut c, 800, 200);
            c.handle(Event::PointerDown {
                local: PhysPoint { x: 290, y: 40 }, button: Button::Primary,
                hit: Some(HitAction::ScrollThumb { travel: 120 }), text: None,
            });
            let commands = if dismiss {
                c.handle(Event::PointerDownOutside)
            } else {
                c.handle(Event::DupesChecked {
                    generation: 1, session: test_session(), dupes: Some(HashSet::new()),
                })
            };
            assert!(commands.contains(&Command::SetDragging(false)));
            if dismiss {
                assert!(c.popup().is_none());
                assert!(c.handle(Event::PointerMoved {
                    local: PhysPoint { x: 290, y: 100 }, text: None,
                }).is_empty());
            } else {
                c.handle(placed(POPUP, 800, 200));
                c.handle(Event::PointerMoved {
                    local: PhysPoint { x: 290, y: 100 }, text: None,
                });
                assert_eq!(0, c.popup().unwrap().scroll);
            }
        }
    }

    #[test]
    fn the_wheel_scrolls_by_whole_notches_and_clamps() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 400, 200);
        assert_eq!(
            vec![Command::RepaintPopup { scroll: SCROLL_STEP_PX, show_back: false }],
            c.handle(Event::Scrolled { notches: -1 })
        );
        // A move past the bottom clamps to the end.
        c.handle(Event::Scrolled { notches: -100 });
        assert_eq!(200, c.popup().expect("placed").scroll);
        // Zero is the top offset.
        c.handle(Event::Scrolled { notches: 100 });
        assert_eq!(0, c.popup().expect("placed").scroll);
        // No offset change produces no repaint.
        assert!(c.handle(Event::Scrolled { notches: 5 }).is_empty());
    }

    // -- click actions --

    #[test]
    fn a_drill_down_answer_pushes_history_and_back_pops_it() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 700, 200);
        c.handle(Event::Scrolled { notches: -2 });
        c.surface.as_mut().expect("card A surface").selection.card_mut(0).replace(SelRange {
            start: TextAddr { entry: 0, addr: crate::select::DocAddr::START },
            end: TextAddr { entry: 0, addr: crate::select::DocAddr::END },
        });
        let parent_selection = c.selection().expect("card A selection").clone();
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("\u{732B}".into())),
        );
        let out = c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("\u{5B57}"))),
        });
        assert_eq!(
            out,
            vec![
                Command::ShowPopup {
                    presentation: Box::new(presentation_of("\u{5B57}")),
                    anchor: ANCHOR,
                    scroll: 0,
                    show_back: true,
                },
                Command::SetBackArmed(true),
            ]
        );
        c.handle(placed(POPUP, 200, 200));
        assert!(c.popup().expect("placed").show_back);
        c.surface
            .as_mut()
            .expect("card B surface")
            .selection
            .card_mut(0)
            .replace(SelRange {
                start: TextAddr {
                    entry: 0,
                    addr: crate::select::DocAddr::START,
                },
                end: TextAddr {
                    entry: 0,
                    addr: crate::select::DocAddr::END,
                },
            });

        let out = c.handle(Event::BackRequested);
        assert_eq!(
            out,
            vec![
                Command::ShowPopup {
                    presentation: Box::new(presentation_of("\u{732B}")),
                    anchor: ANCHOR,
                    scroll: 96,
                    show_back: false,
                },
                Command::SetBackArmed(false),
            ]
        );
        c.handle(placed(POPUP, 700, 200));
        assert!(!c.popup().expect("placed").show_back);
        assert_eq!(c.popup().expect("placed").scroll, 96);
        assert_eq!(c.selection().expect("card A selection"), &parent_selection);
        assert!(c.handle(Event::BackRequested).is_empty());
    }

    /// Both the hotkey and the click update a dupe when the setting is on.
    #[test]
    fn a_duplicate_add_updates_the_card_when_the_setting_is_on() {
        let config = ControllerConfig {
            anki_enabled: true,
            overwrite_duplicates: true,
            ..cfg()
        };
        let mut c = test_controller(config.clone());
        shown(&mut c);
        a_known_dupe(&mut c);
        let out = c.handle(Event::AddRequested);
        assert!(out.iter().any(|cmd| matches!(cmd, Command::AddNote { .. })));
        assert!(c.anki().expect("shown").saving);

        let mut c = test_controller(config);
        shown(&mut c);
        a_known_dupe(&mut c);
        let below = PhysPoint { x: 10, y: POPUP.h + 5 };
        let out = click(&mut c, below, Button::Primary, None);
        assert!(out.iter().any(|cmd| matches!(cmd, Command::AddNote { .. })));
        assert!(c.anki().expect("shown").saving);
    }

    #[test]
    fn a_sentence_result_completes_the_add_with_the_new_sentence() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown(&mut c);
        let request_id = match &c.handle(Event::AddRequested)[1] {
            Command::RequestSentence { id, .. } => *id,
            command => panic!("expected a sentence request, got {command:?}"),
        };
        let sentence = "今日はいい天気ですね。".to_string();
        let out = c.handle(Event::LookupResult {
            id: request_id,
            outcome: LookupOutcome::Sentence(Some(sentence.clone())),
        });
        let fields = match out.as_slice() {
            [Command::AddNote { fields, .. }, Command::SyncAnkiButton] => fields,
            commands => panic!("expected the add tail, got {commands:?}"),
        };
        assert_eq!(fields.get("sentence"), Some(&sentence));
    }

    // -- duplicate checks --

    // -- Worker outcomes --

    #[test]
    fn a_superseded_answer_is_ignored() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } });
        let stale = c.latest;
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 100, y: 100 } });
        assert!(c
            .handle(Event::LookupResult { id: stale, outcome: LookupOutcome::Hide })
            .is_empty());
    }

    // -- configuration reload --

    // -- tray and quit --

    // -- hold geometry --

    // -- selection and Controller behavior --
    #[test]
    fn a_drag_over_two_glosses_updates_the_public_selection() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        let card = gloss_card("first", "second");
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));
        let first_path = crate::select::DocAddr {
            path: crate::dict::gloss::NodePath::ROOT.child(0).unwrap(),
            byte: 0,
        };
        let second_path = crate::select::DocAddr {
            path: crate::dict::gloss::NodePath::ROOT.child(1).unwrap(),
            byte: 0,
        };
        let first = TextAddr { entry: 0, addr: first_path };
        let second_end = TextAddr { entry: 0, addr: crate::select::DocAddr { path: second_path.path, byte: 1 } };
        c.handle(Event::PointerDown {
            local: PhysPoint { x: 0, y: 0 },
            button: Button::Primary,
            hit: None,
            text: Some(first),
        });
        let out = c.handle(Event::PointerMoved {
            local: PhysPoint { x: 5, y: 0 },
            text: Some(second_end),
        });
        assert!(out.contains(&Command::SetDragging(true)));
        let selected = c
            .selection()
            .and_then(|selection| selection.card(0))
            .expect("drag selection");
        assert_eq!(selected.items().len(), 1);
        assert_eq!(selected.items()[0].start, first);
        assert_eq!(selected.items()[0].end.addr.path, second_path.path);
        assert!(selected.items()[0].end.addr.byte > 0);
        assert!(c
            .handle(Event::PointerUp { local: PhysPoint { x: 5, y: 0 }, button: Button::Primary })
            .contains(&Command::SetDragging(false)));
    }

    /// One payload rule covers the expression fallback, block trim, and empty card.
    #[test]
    fn the_note_payload_trims_to_the_first_dict_and_carries_the_sentence() {
        let block = |name: &str, gloss: &str| {
            GlossBlock::parse(name, &serde_json::json!([gloss]).to_string())
        };
        let mut p = Presentation {
            top: Some(Card {
                written: None,
                reading: Some("\u{306D}\u{3053}".into()),
                blocks: vec![block("A", "cat"), block("B", "feline")],
                ..card("\u{732B}")
            }),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: Some("\u{732B}\u{304C}\u{3044}\u{308B}".into()),
            surface: None,
        };

        // The `reading` value replaces the missing `written` value.
        let empty = CardSelection::default();
        let (expr, fields) = note_payload(&p, false, true, &empty, Separator::Ellipsis);
        assert_eq!("\u{306D}\u{3053}", expr);
        assert_eq!(Some(&"\u{732B}\u{304C}\u{3044}\u{308B}".to_string()), fields.get("sentence"));
        let both = fields.get("glossary").expect("glossary field");
        assert!(both.contains("cat") && both.contains("feline"), "{both}");

        // The first Dictionary excludes all later blocks.
        let (_, trimmed) = note_payload(&p, true, true, &empty, Separator::Ellipsis);
        let first = trimmed.get("glossary").expect("glossary field");
        assert!(first.contains("cat") && !first.contains("feline"), "{first}");

        // Without a top card, the payload has nothing to add.
        p.top = None;
        assert_eq!(
            (String::new(), HashMap::new()),
            note_payload(&p, false, true, &empty, Separator::Ellipsis),
        );
    }

    #[test]
    fn screen_cursor_samples_do_not_override_popup_entry() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        hover_child(&mut c, "犬");
        let child = c.popup().unwrap().popup;
        c.handle(Event::PopupEntered { depth: usize::MAX });
        c.handle(Event::PopupEntered { depth: 1 });
        for pos in [
            PhysPoint { x: child.x + 10, y: child.y - 2 },
            PhysPoint { x: child.x + 10, y: child.y - 1 },
            PhysPoint { x: child.x + 10, y: child.y },
        ] {
            let commands = c.handle(Event::CursorMoved { pos });
            assert!(!commands.iter().any(|command| matches!(command,
                Command::RestorePopup { .. } | Command::HidePopup | Command::RequestLookup { .. }
            )), "{pos:?}: {commands:?}");
            assert_eq!(c.popup_depth(), 1);
            assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("犬"));
        }
        c.handle(Event::PopupEntered { depth: usize::MAX });
        assert!(c.handle(Event::PopupEntered { depth: 0 })
            .contains(&Command::RestorePopup { depth: 0 }));
        assert_eq!(c.popup_depth(), 0);
    }

    #[test]
    fn crossing_parent_text_to_a_child_keeps_the_child_open() {
        for (child, points) in [
            (PhysRect { x: 150, y: 271, w: 300, h: 200 },
                [(160, 240), (175, 250), (180, 265), (180, 272)]),
            (PhysRect { x: -150, y: -11, w: 300, h: 200 },
                [(140, 220), (135, 205), (130, 195), (130, 185)]),
        ] {
            let mut c = test_controller(cfg());
            shown(&mut c);
            let session = c.popup().unwrap().session.clone();
            let id = popup_shortcut(&mut c, 0, session.clone(), "犬");
            c.handle(Event::LookupResult {
                id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
            });
            c.handle(placed(child, 200, 200));
            for (x, y) in points {
                let point = PhysPoint { x, y };
                let mut commands = c.handle(Event::CursorMoved { pos: point });
                if child.contains(point) {
                    commands.extend(c.handle(Event::PopupEntered { depth: 1 }));
                } else {
                    commands.extend(c.handle(Event::PopupHoverAt {
                        depth: 0, local: PhysPoint { x: x - POPUP.x, y: y - POPUP.y },
                        query: Some("別の言葉".into()),
                    }));
                }
                for _ in 0..20 {
                    commands.extend(c.handle(Event::Tick { cursor: point, button_h: 0 }));
                }
                assert!(!commands.iter().any(|command| matches!(command,
                    Command::RestorePopup { .. } | Command::RequestLookup { .. }
                    | Command::RequestDrillDown { .. } | Command::HidePopup
                )), "{point:?}: {commands:?}");
                assert_eq!(c.popup_depth(), 1);
                assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("犬"));
                assert_eq!(c.popup().unwrap().session, &session);
            }
            assert!(c.handle(Event::PopupEntered { depth: 0 })
                .contains(&Command::RestorePopup { depth: 0 }));
        }
    }

    fn nested_test_sessions() -> (ProfileSession, ProfileSession, ProfileSession) {
        let mut config = crate::config::Config::default();
        let default_profile = config.default_profile.clone();
        let child_settings = {
            let profile = config.profiles.iter_mut()
                .find(|profile| profile.id == default_profile)
                .expect("default profile exists");
            let crate::config::ProfileData::Full { settings } = &mut profile.data else {
                panic!("default profile has full settings");
            };
            settings.nested_profile = Some("child".into());
            let mut child = (**settings).clone();
            child.popup.summary_chars = 91;
            child.nested_profile = Some("grandchild".into());
            child
        };
        let mut grandchild_settings = child_settings.clone();
        grandchild_settings.popup.summary_chars = 92;
        grandchild_settings.nested_profile = None;
        config.profiles.push(crate::config::Profile {
            id: "child".into(),
            name: "Child".into(),
            data: crate::config::ProfileData::Full { settings: Box::new(child_settings) },
        });
        config.profiles.push(crate::config::Profile {
            id: "grandchild".into(),
            name: "Grandchild".into(),
            data: crate::config::ProfileData::Full { settings: Box::new(grandchild_settings) },
        });
        let catalog = crate::config::ProfileCatalog::new(&config, &[])
            .expect("nested test config builds");
        let root = catalog.session(None).expect("default profile exists");
        let child = root.nested();
        let grandchild = child.nested();
        (root, child, grandchild)
    }
}
