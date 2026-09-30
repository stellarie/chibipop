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
            || (self.trigger_mode() == TriggerMode::Press && self.surface.is_some())
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
        let session = self.request_session_default();
        self.set_profile_config(&session);
        if let Some(s) = self.surface.as_ref() {
            out.push(self.repaint(s.scroll));
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

    fn push_hover(&mut self, mut presentation: Presentation, local: PhysPoint, session: ProfileSession) -> Vec<Command> {
        let Some(parent) = self.surface.as_ref() else { return Vec::new() };
        let Some(placed) = parent.placed else { return Vec::new() };
        let source = parent.source;
        if source == RootSource::SelectedText {
            presentation.sentence = parent.presentation.sentence.clone();
            presentation.surface = None;
        }
        let same_word = parent.session == session && presentation.top.as_ref().zip(parent.presentation.top.as_ref())
            .is_some_and(|(a, b)| a.written == b.written && a.reading == b.reading);
        if presentation.top.is_none() || same_word
            || self.parents.len() >= 16
        {
            return Vec::new();
        }
        let anchor = PhysRect {
            x: placed.popup.x.saturating_add(local.x),
            y: placed.popup.y.saturating_add(local.y), w: 1, h: 1,
        };
        let mut parent = self.surface.take().expect("parent exists");
        parent.gesture.reset();
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
        self.request_session = None;
        id
    }
    fn request_session_default(&self) -> ProfileSession {
        self.active_bind.as_ref().map(|bind| &bind.session)
            .or_else(|| self.chain_bind.as_ref().map(|bind| &bind.session))
            .or_else(|| self.surface.as_ref().map(|surface| &surface.session))
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
        let command = if mode == TriggerMode::Press {
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
        let anki = s.session.config().anki.enabled;

        match hit {
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
        let sentence_probe = sentence_probe && !self.selected_text_active();
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
        if matches!(self.trigger_mode(), TriggerMode::Live | TriggerMode::Press) {
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
        if let Some(depth) = self.popup_at(pos) {
            return self.popup_entered(depth);
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
        if self.selected_text_active() {
            return false;
        }
        let live = match self.trigger_mode() {
            TriggerMode::Live => true,
            TriggerMode::Toggle => self.trigger_held,
            _ => false,
        };
        live && self.parents.is_empty() && self.hover_request.is_none()
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

    /// Press mode has no sticky region. A press over the popup is a deliberate
    /// lookup that the mask turns into a miss, which hides the popup.
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
        if self.hover_request.as_ref().is_some_and(|(request, _, _)| *request == id) {
            let (_, local, session) = self.hover_request.take().expect("matching request");
            return match outcome {
                LookupOutcome::DrillDown(presentation) => self.push_hover(*presentation, local, session),
                LookupOutcome::Failed(message) => vec![Command::WarnLookupFailed(message)],
                _ => Vec::new(),
            };
        }
        let Some(request) = self.request_session.take().filter(|request| request.id == id) else {
            return Vec::new();
        };
        match outcome {
            LookupOutcome::Hide => self.hide(),
            LookupOutcome::Failed(message) => {
                let mut out = vec![Command::WarnLookupFailed(message)];
                out.extend(self.hide());
                out
            }
            LookupOutcome::DrillDown(presentation) => self.push_drilldown(*presentation, request.session),
            LookupOutcome::Ready { presentation, anchor, orientation, matched, scan } => {
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
        let session = self.active_bind.as_ref().map(|bind| bind.session.clone())
            .unwrap_or_else(|| self.future_session.clone());
        self.set_profile_config(&session);
        vec![Command::HidePopup, Command::SetBackArmed(false)]
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
        if matches!(kind, PlaceKind::Fresh) {
            s.placed = None;
        }
        self.awaiting = Some(kind);
        let s = self.surface.as_ref().expect("checked above");
        vec![Command::ShowPopup {
            presentation: Box::new(s.presentation.clone()),
            anchor: s.anchor,
            scroll: s.scroll,
            show_back: self.has_history(),
        }]
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
            let s = self.surface.as_mut().expect("checked above");
            s.generation = generation;
            if !exprs.is_empty() {
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
    use crate::present::{Card, CollapsedRow, GlossBlock};
    use std::cell::RefCell;

    thread_local! {
        static CURRENT_TEST_SESSION: RefCell<Option<ProfileSession>> = const { RefCell::new(None) };
    }

    fn session_for_controller_cfg(cfg: &ControllerConfig) -> ProfileSession {
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
        }
        config.debug.show_lookup_log = cfg.log_lookups;
        crate::config::ProfileCatalog::new(&config, &[])
            .expect("test config builds")
            .session(None)
            .expect("default profile exists")
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

    fn add_note_request(commands: &[Command]) -> (RequestId, ProfileSession) {
        commands.iter().find_map(|command| match command {
            Command::AddNote { id, session, .. } => Some((*id, session.clone())),
            _ => None,
        }).expect("the add command")
    }

    fn note_added(
        c: &mut Controller,
        id: RequestId,
        expr: &str,
        session: ProfileSession,
    ) -> Vec<Command> {
        c.handle(Event::NoteAdded { id, expr: expr.to_string(), session, failed: false })
    }

    fn note_written(
        c: &mut Controller,
        id: RequestId,
        expr: &str,
        session: ProfileSession,
        status: NoteWriteStatus,
    ) -> Vec<Command> {
        c.handle(Event::NoteWritten { id, expr: expr.to_string(), session, status })
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

    fn cursor_lookup_request(c: &mut Controller, pos: PhysPoint) -> RequestId {
        c.handle(Event::CursorMoved { pos })
            .into_iter()
            .find_map(|command| match command {
                Command::RequestLookup { id, .. } => Some(id),
                _ => None,
            })
            .expect("cursor movement starts a lookup")
    }

    fn selected_text_request(c: &mut Controller, pos: PhysPoint) -> RequestId {
        let session = c.request_session_default();
        remember_test_session(&session);
        match c.handle(Event::SelectedTextRequested { pos, session }).as_slice() {
            [Command::ReadSelectedText { id, .. }] => *id,
            commands => panic!("unexpected selection commands: {commands:?}"),
        }
    }

    fn shown_selected(c: &mut Controller, text: &str) {
        let id = selected_text_request(c, PhysPoint { x: 40, y: 50 });
        c.handle(Event::SelectedTextReady { id, text: Some(text.into()), bounds: None });
        c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of(text))),
        });
        c.handle(placed(POPUP, 200, 200));
    }

    fn hover_child(c: &mut Controller, word: &str) {
        let id = hover_request(c, word);
        let commands = c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of(word))) });
        assert!(commands.contains(&Command::PushPopup));
        let depth = c.popup_depth() as i32;
        c.handle(placed(PhysRect { x: 100 + depth * 150, y: 160 + depth * 60, ..POPUP }, 700, 200));
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
    fn hover_stationary_parent_does_not_immediately_close_its_child() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        hover_child(&mut c, "犬");
        assert!(c.handle(Event::PopupEntered { depth: 0 }).is_empty());
        assert_eq!(c.popup_depth(), 1);
        c.handle(Event::PopupEntered { depth: 1 });
        assert!(c.handle(Event::PopupEntered { depth: 0 }).contains(&Command::RestorePopup { depth: 0 }));
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
    fn nested_profiles_remain_retained_across_reload_until_the_chain_ends() {
        let (root, child, grandchild) = nested_test_sessions();
        let root_cfg = ControllerConfig::for_profile(root.config());
        remember_test_session(&root);
        let mut c = Controller::new(root_cfg.clone(), root.clone());
        shown(&mut c);

        let child_request = click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("犬".into())),
        )
        .into_iter()
        .find_map(|command| match command {
            Command::RequestDrillDown { id, session, .. } => {
                assert_eq!(session, child);
                Some(id)
            }
            _ => None,
        })
        .expect("the child profile lookup");
        c.handle(Event::LookupResult {
            id: child_request,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(c.surface.as_ref().unwrap().session, child);
        assert_eq!(c.cfg.summary_chars, 91);

        let default_cfg = ControllerConfig { summary_chars: 75, ..cfg() };
        let default_session = session_for_controller_cfg(&default_cfg);
        remember_test_session(&default_session);
        c.handle(Event::ConfigReloaded {
            cfg: Box::new(default_cfg),
            session: default_session.clone(),
        });

        let grandchild_request = click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("鳥".into())),
        )
        .into_iter()
        .find_map(|command| match command {
            Command::RequestDrillDown { id, session, .. } => {
                assert_eq!(session, grandchild);
                Some(id)
            }
            _ => None,
        })
        .expect("the grandchild profile lookup");
        c.handle(Event::LookupResult {
            id: grandchild_request,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("鳥"))),
        });
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(c.surface.as_ref().unwrap().session, grandchild);
        assert_eq!(c.cfg.summary_chars, 92);

        c.handle(Event::BackRequested);
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(c.surface.as_ref().unwrap().session, child);
        assert_eq!(c.cfg.summary_chars, 91);
        c.handle(Event::BackRequested);
        c.handle(placed(POPUP, 200, 200));
        assert_eq!(c.surface.as_ref().unwrap().session, root);
        assert_eq!(c.cfg.summary_chars, root_cfg.summary_chars);

        c.handle(Event::DismissRequested);
        assert_eq!(c.cfg_session, default_session);
        let resumed = c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        assert!(resumed.iter().any(|command| matches!(
            command,
            Command::RequestLookup { session, .. } if session == &default_session
        )));
    }

    #[test]
    fn parent_click_is_armed_and_activates_without_ever_entering_child() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        hover_child(&mut c, "犬");
        let point = PhysPoint { x: POPUP.x + 10, y: POPUP.y + 10 };
        let commands = c.handle(Event::Tick { cursor: point, button_h: 0 });
        assert!(commands.contains(&Command::SetClickArmed(true)));
        assert_eq!(c.popup_depth(), 1);
        assert!(c.handle(Event::PopupActivated { depth: 0 }).contains(&Command::RestorePopup { depth: 0 }));
        let commands = c.handle(Event::PointerDown {
            local: PhysPoint { x: 10, y: 10 }, button: Button::Primary,
            hit: Some(HitAction::DrillDown("魚".into())), text: None,
        });
        assert!(commands.iter().any(|command| matches!(command, Command::RequestDrillDown { text, .. } if text == "魚")));
        assert_eq!(c.popup_depth(), 0);
        assert_eq!(c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(), Some("猫"));
    }

    #[test]
    fn parent_scroll_is_armed_and_changes_parent_without_entering_child() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 700, 200);
        hover_child(&mut c, "犬");
        c.surface.as_mut().unwrap().placed.as_mut().unwrap().content_h = 200;
        let point = PhysPoint { x: POPUP.x + 10, y: POPUP.y + 10 };
        let commands = c.handle(Event::Tick { cursor: point, button_h: 0 });
        assert!(commands.contains(&Command::SetScrollArmed(true)));
        assert_eq!(c.popup_depth(), 1);
        c.handle(Event::PopupActivated { depth: 0 });
        c.handle(Event::Scrolled { notches: -1 });
        assert_eq!(c.popup_depth(), 0);
        assert_eq!(c.popup().unwrap().scroll, SCROLL_STEP_PX);
        assert_eq!(c.popup().unwrap().content_h, 700);
    }

    #[test]
    fn parent_hover_preserves_same_item_but_activates_another_item() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        hover_child(&mut c, "犬");
        let local = PhysPoint { x: 50, y: 70 };
        assert!(c.handle(Event::PopupHoverAt { depth: 0, local, query: Some("犬".into()) }).is_empty());
        assert_eq!(c.popup_depth(), 1);
        assert!(c.handle(Event::PopupHoverAt { depth: 0, local, query: None }).is_empty());
        assert_eq!(c.popup_depth(), 1);
        assert!(c.handle(Event::PopupHoverAt { depth: 0, local, query: Some("鳥".into()) })
            .contains(&Command::RestorePopup { depth: 0 }));
        assert_eq!(c.popup_depth(), 0);
        assert!(c.hover_candidate.as_ref().is_some_and(|(query, _, _)| query == "鳥"));
    }

    #[test]
    fn deliberate_parent_activation_does_not_retarget_an_active_drag() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        hover_child(&mut c, "犬");
        c.handle(Event::PointerDown { local: PhysPoint { x: 10, y: 10 }, button: Button::Primary, hit: None, text: None });
        assert!(c.handle(Event::PopupActivated { depth: 0 }).is_empty());
        assert!(c.handle(Event::PopupHoverAt { depth: 0, local: PhysPoint { x: 50, y: 70 }, query: Some("鳥".into()) }).is_empty());
        assert_eq!(c.popup_depth(), 1);
        c.handle(Event::PointerUp { local: PhysPoint { x: 10, y: 10 }, button: Button::Primary });
        assert!(c.handle(Event::PopupActivated { depth: 0 }).contains(&Command::RestorePopup { depth: 0 }));
    }

    #[test]
    fn hover_parent_analysis_restarts_only_for_the_restored_top_card() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        shown(&mut c);
        let gloss = GlossBlock::parse("Test", r#"["親の文章"]"#);
        c.surface.as_mut().unwrap().presentation.top.as_mut().unwrap().blocks = vec![gloss];
        hover_child(&mut c, "犬");
        let child_generation = c.surface.as_ref().unwrap().analysis_generation;
        let commands = c.handle(Event::BackRequested);
        let (generation, texts) = commands.iter().find_map(|cmd| match cmd {
            Command::RequestAnalysis { generation, texts } => Some((*generation, texts)), _ => None,
        }).expect("restored parent must request interrupted analysis");
        assert!(generation > child_generation);
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "親の文章");
        assert!(c.handle(Event::AnalysisReady { generation: child_generation, words: WordMap::new() }).is_empty());
        assert!(c.surface.as_ref().unwrap().analysis.is_none());
    }

    #[test]
    fn hover_parent_anki_replies_cannot_mark_the_child() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        shown(&mut c);
        let generation = c.surface.as_ref().unwrap().generation;
        let analysis_generation = c.surface.as_ref().unwrap().analysis_generation;
        let write_commands = c.handle(Event::AddRequested);
        let (id, session) = add_note_request(&write_commands);
        hover_child(&mut c, "犬");
        let wrong_session = session_for_controller_cfg(&ControllerConfig {
            anki_enabled: true,
            ..cfg()
        });
        c.handle(Event::DupesChecked {
            generation,
            dupes: Some(HashSet::from(["猫".into()])),
            session: wrong_session,
        });
        assert!(!c.parents[0].anki.dupes.contains("猫"));
        let words = WordMap::new();
        c.handle(Event::AnalysisReady { generation: analysis_generation, words: words.clone() });
        c.handle(Event::DupesChecked {
            generation,
            dupes: Some(HashSet::from(["猫".into()])),
            session: test_session(),
        });
        note_added(&mut c, id, "猫", session);
        assert!(c.parents[0].anki.dupes.contains("猫"));
        assert!(c.parents[0].anki.added.contains("猫"));
        assert_eq!(c.parents[0].analysis, Some((analysis_generation, words)));
        assert!(!c.parents[0].anki.adding);
        assert!(c.anki().unwrap().added.is_empty());
    }

    #[test]
    fn hover_replies_after_leaving_back_and_dismiss_are_rejected() {
        for action in [0, 1, 2] {
            let mut c = test_controller(cfg());
            shown(&mut c);
            hover_child(&mut c, "犬");
            let id = hover_request(&mut c, "鳥");
            match action {
                0 => { c.handle(Event::PopupHover { local: PhysPoint { x: 0, y: 0 }, query: None }); }
                1 => { c.handle(Event::BackRequested); }
                _ => { c.handle(Event::DismissRequested); }
            }
            let before = c.surface.clone();
            assert!(c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("鳥"))) }).is_empty());
            assert_eq!(c.surface, before);
        }
    }

    #[test]
    fn hover_depth_is_bounded_and_dismiss_retires_every_descendant() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        for depth in 0..16 { hover_child(&mut c, &format!("語{depth}")); }
        let id = hover_request(&mut c, "限界");
        assert!(c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("限界"))) }).is_empty());
        assert_eq!(c.popup_depth(), 16);
        assert_eq!(c.popup_rects().len(), 17);
        assert!(c.handle(Event::DismissRequested).contains(&Command::HidePopup));
        assert!(c.popup_rects().is_empty());
        assert_eq!(c.popup_depth(), 0);
        assert!(c.hover_request.is_none());
    }

    #[test]
    fn hover_same_query_and_same_headword_do_not_open_again() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = hover_request(&mut c, "猫");
        assert!(c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("猫"))) }).is_empty());
        c.handle(Event::PopupHover { local: PhysPoint { x: 51, y: 70 }, query: Some("猫".into()) });
        assert!((0..20).flat_map(|_| c.handle(Event::GestureTick)).all(|cmd| !matches!(cmd, Command::RequestDrillDown { .. })));
        assert_eq!(c.popup_depth(), 0);
    }

    #[test]
    fn hover_failure_and_failed_placement_keep_the_parent() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 700, 200);
        c.handle(Event::Scrolled { notches: -2 });
        let id = hover_request(&mut c, "失敗");
        let before = c.surface.clone();
        let commands = c.handle(Event::LookupResult { id, outcome: LookupOutcome::Failed("missing dictionary".into()) });
        assert_eq!(commands, vec![Command::WarnLookupFailed("missing dictionary".into())]);
        assert_eq!(c.surface, before);
        let id = hover_request(&mut c, "犬");
        let before = c.surface.clone();
        c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))) });
        assert!(c.handle(Event::PopupPlaceFailed).contains(&Command::RestorePopup { depth: 0 }));
        assert_eq!(c.surface, before);
        assert_eq!(c.popup_depth(), 0);
    }

    #[test]
    fn hover_mouse_press_and_selection_drag_cancel_pending_lookup() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = hover_request(&mut c, "犬");
        c.handle(Event::PointerDown { local: PhysPoint { x: 1, y: 1 }, button: Button::Primary, hit: None, text: None });
        assert!(c.handle(Event::LookupResult { id, outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))) }).is_empty());
        c.surface.as_mut().unwrap().last_drag_point = Some(PhysPoint { x: 10, y: 10 });
        c.handle(Event::PopupHover { local: PhysPoint { x: 20, y: 20 }, query: Some("鳥".into()) });
        assert!((0..20).flat_map(|_| c.handle(Event::GestureTick)).all(|cmd| !matches!(cmd, Command::RequestDrillDown { .. })));
    }

    #[test]
    fn dismiss_requested_works_in_every_trigger_mode_and_invalidates_root_reply() {
        for mode in [
            TriggerMode::Live,
            TriggerMode::Press,
            TriggerMode::HoldKey,
            TriggerMode::Toggle,
        ] {
            let mut c = test_controller(if mode == TriggerMode::Live { cfg() } else { press_cfg() });
            bind_down(&mut c, "dismiss", mode, PhysPoint { x: 110, y: 110 });
            shown(&mut c);
            hover_child(&mut c, "犬");
            let id = c.latest_lookup;
            let commands = c.handle(Event::DismissRequested);
            for command in [Command::SetDragging(false), Command::SetScrollArmed(false),
                Command::SetClickArmed(false), Command::SetAddArmed(false), Command::DiscardScroll]
            {
                assert!(commands.contains(&command));
            }
            assert!(c.popup_rects().is_empty());
            assert!(c.handle(ready_id(id, "古い", ANCHOR)).is_empty());
            assert!(c.popup().is_none());
        }
    }

    #[test]
    fn selected_text_bounds_become_the_popup_anchor() {
        let mut c = test_controller(cfg());
        let bounds = PhysRect { x: 300, y: 200, w: 80, h: 32 };
        let id = selected_text_request(&mut c, PhysPoint { x: 900, y: 800 });
        c.handle(Event::SelectedTextReady { id, text: Some("猫".into()), bounds: Some(bounds) });
        let commands = c.handle(Event::LookupResult { id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("猫"))) });
        assert!(commands.iter().any(|command| matches!(command, Command::ShowPopup { anchor, .. } if *anchor == bounds)));
    }

    #[test]
    fn selected_sentence_search_bypasses_lookup_and_rejects_stale_capture() {
        let mut config = cfg();
        config.selected_text_sentence_search = true;
        let mut c = test_controller(config);
        let old = selected_text_request(&mut c, PhysPoint { x: 0, y: 0 });
        let id = selected_text_request(&mut c, PhysPoint { x: 0, y: 0 });
        assert!(c.handle(Event::SelectedTextReady { id: old, text: Some("古い".into()), bounds: None }).is_empty());
        let commands = c.handle(Event::SelectedTextReady { id, text: Some("猫がいる。".into()), bounds: None });
        assert!(commands.contains(&Command::OpenSentenceSearch {
            text: "猫がいる。".into(),
            session: test_session(),
        }));
        assert!(!commands.iter().any(|command| matches!(command, Command::RequestDrillDown { .. } | Command::RequestLookup { .. })));
        assert!(!c.watches_outside_clicks());
    }

    #[test]
    fn selected_popup_outside_click_dismisses_every_mode_and_pending_reply() {
        for mode in [TriggerMode::Live, TriggerMode::HoldKey, TriggerMode::Toggle, TriggerMode::Press] {
            let mut c = test_controller(if mode == TriggerMode::Live { cfg() } else { press_cfg() });
            bind_down(&mut c, "selected", mode, PhysPoint { x: 110, y: 110 });
            shown_selected(&mut c, "猫");
            assert!(c.watches_outside_clicks());
            assert!(c.handle(Event::PointerDownOutside).contains(&Command::HidePopup));
            assert!(!c.is_shown());
            let id = selected_text_request(&mut c, PhysPoint { x: 0, y: 0 });
            c.handle(Event::PointerDownOutside);
            assert!(c.handle(Event::SelectedTextReady { id, text: Some("遅い".into()), bounds: None }).is_empty());
        }
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

    #[test]
    fn selected_text_rejects_empty_oversized_and_stale_capture_results() {
        let mut c = test_controller(cfg());
        let pos = PhysPoint { x: 20, y: 30 };
        let old = selected_text_request(&mut c, pos);
        let current = selected_text_request(&mut c, pos);
        assert!(c.handle(Event::SelectedTextReady {
            id: old,
            text: Some("古い".into()), bounds: None }).is_empty());
        assert!(c.handle(Event::SelectedTextReady {
            id: current,
            text: Some("  ".into()), bounds: None }).is_empty());
        let absent = selected_text_request(&mut c, pos);
        assert!(c.handle(Event::SelectedTextReady { id: absent, text: None, bounds: None }).is_empty());
        let oversized = selected_text_request(&mut c, pos);
        assert!(c.handle(Event::SelectedTextReady {
            id: oversized,
            text: Some("a".repeat(MAX_SELECTED_TEXT_BYTES + 1)), bounds: None }).is_empty());
        assert!(!c.is_shown());
    }

    #[test]
    fn selected_text_stays_open_and_never_follows_ocr_trigger_state() {
        for mode in [
            TriggerMode::Live,
            TriggerMode::Press,
            TriggerMode::HoldKey,
            TriggerMode::Toggle,
        ] {
            let mut c = test_controller(if mode == TriggerMode::Live { cfg() } else { press_cfg() });
            bind_down(&mut c, "selected", mode, PhysPoint { x: 40, y: 50 });
            let id = selected_text_request(&mut c, PhysPoint { x: 40, y: 50 });
            c.handle(Event::SelectedTextReady { id, text: Some("猫".into()), bounds: None });
            c.handle(Event::LookupResult {
                id,
                outcome: LookupOutcome::DrillDown(Box::new(presentation_of("猫"))),
            });
            c.handle(placed(POPUP, 200, 200));
            if mode == TriggerMode::Toggle {
                bind_down(&mut c, "selected", mode, PhysPoint { x: 40, y: 50 });
            } else {
                bind_up(&mut c, "selected");
            }
            assert!(c.handle(Event::CursorMoved {
                pos: PhysPoint { x: 800, y: 700 },
            }).is_empty());
            assert!(c.handle(Event::DwellElapsed).is_empty());
            assert!(c.is_shown());
        }
    }

    #[test]
    fn explicit_ocr_triggers_replace_selected_text_roots() {
        for mode in [TriggerMode::HoldKey, TriggerMode::Toggle] {
            let mut c = test_controller(press_cfg());
            shown_selected(&mut c, "猫");
            let commands = bind_down(&mut c, "ocr", mode, PhysPoint { x: 800, y: 700 });
            let id = commands.iter().find_map(|command| match command {
                Command::RequestLookup { id, .. } => Some(*id),
                _ => None,
            }).expect("explicit trigger must request OCR");
            c.handle(ready_id(id, "犬", ANCHOR));
            assert_eq!(RootSource::Ocr, c.surface.as_ref().expect("OCR popup").source);
            if mode == TriggerMode::Toggle {
                assert!(bind_down(&mut c, "ocr", mode, PhysPoint { x: 800, y: 700 })
                    .contains(&Command::HidePopup));
            } else {
                assert!(bind_up(&mut c, "ocr").contains(&Command::HidePopup));
            }
        }

        let mut c = test_controller(press_cfg());
        shown_selected(&mut c, "猫");
        let commands = bind_down(&mut c, "ocr", TriggerMode::Press, PhysPoint { x: 800, y: 700 });
        let id = commands.iter().find_map(|command| match command {
            Command::RequestLookup { id, .. } => Some(*id),
            _ => None,
        }).expect("press must request OCR");
        c.handle(ready_id(id, "犬", ANCHOR));
        assert_eq!(RootSource::Ocr, c.surface.as_ref().expect("OCR popup").source);
        bind_up(&mut c, "ocr");
    }

    #[test]
    fn unavailable_selected_text_clears_the_prior_selected_root() {
        for text in [None, Some("  ".to_string())] {
            let mut c = test_controller(cfg());
            shown_selected(&mut c, "猫");
            let id = selected_text_request(&mut c, PhysPoint { x: 60, y: 70 });
            let commands = c.handle(Event::SelectedTextReady { id, text, bounds: None });
            assert!(commands.contains(&Command::HidePopup));
            assert!(!c.is_shown());
        }
    }

    #[test]
    fn dismissed_and_superseded_selected_text_work_is_rejected_but_reload_preserves_it() {
        let pos = PhysPoint { x: 70, y: 80 };
        let mut c = test_controller(cfg());
        let dismissed = selected_text_request(&mut c, pos);
        c.handle(Event::DismissRequested);
        assert!(c.handle(Event::SelectedTextReady {
            id: dismissed,
            text: Some("古い".into()), bounds: None }).is_empty());

        let dismissed_lookup = selected_text_request(&mut c, pos);
        c.handle(Event::SelectedTextReady {
            id: dismissed_lookup,
            text: Some("古い".into()), bounds: None });
        c.handle(Event::DismissRequested);
        assert!(c.handle(Event::LookupResult {
            id: dismissed_lookup,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("古い"))),
        }).is_empty());

        let stale_lookup = selected_text_request(&mut c, pos);
        c.handle(Event::SelectedTextReady {
            id: stale_lookup,
            text: Some("古い".into()), bounds: None });
        selected_text_request(&mut c, pos);
        assert!(c.handle(Event::LookupResult {
            id: stale_lookup,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("古い"))),
        }).is_empty());

        let pending_lookup = selected_text_request(&mut c, pos);
        c.handle(Event::SelectedTextReady {
            id: pending_lookup,
            text: Some("古い".into()), bounds: None });
        let original_session = c.request_session.as_ref().expect("selected lookup request").session.clone();
        reload(&mut c, cfg());
        let out = c.handle(Event::LookupResult {
            id: pending_lookup,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("古い"))),
        });
        assert!(out.iter().any(|command| matches!(command, Command::ShowPopup { .. })));
        assert!(c.is_shown());
        assert_eq!(c.surface.as_ref().unwrap().session, original_session);
        assert_ne!(c.surface.as_ref().unwrap().session, test_session());
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

    #[test]
    fn nothing_is_armed_without_a_popup() {
        let mut c = test_controller(cfg());
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 5, y: 5 }, button_h: 0 });
        assert_eq!(
            out,
            vec![
                Command::SetScrollArmed(false),
                Command::SetClickArmed(false),
                Command::SetAddArmed(false),
                Command::SetBackArmed(false),
            ]
        );
    }

    #[test]
    fn the_wheel_arms_only_over_a_scrollable_popup() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        // view == content: no scroll range exists.
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 150, y: 200 }, button_h: 0 });
        assert!(out.contains(&Command::SetScrollArmed(false)));
        assert!(out.contains(&Command::SetClickArmed(true)));

        let mut c = test_controller(cfg());
        shown_sized(&mut c, 500, 200);
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 150, y: 200 }, button_h: 0 });
        assert!(out.contains(&Command::SetScrollArmed(true)));
    }

    #[test]
    fn the_click_arm_covers_the_button_strip_below_the_popup() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let just_below = PhysPoint { x: 150, y: POPUP.y + POPUP.h + 10 };
        let out = c.handle(Event::Tick { cursor: just_below, button_h: 0 });
        assert!(out.contains(&Command::SetClickArmed(false)));
        let out = c.handle(Event::Tick { cursor: just_below, button_h: 40 });
        assert!(out.contains(&Command::SetClickArmed(true)));
    }

    #[test]
    fn the_cursor_shape_is_asked_for_only_over_the_popup() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 150, y: 200 }, button_h: 0 });
        assert!(out.contains(&Command::SetCursorShape {
            local: PhysPoint { x: 50, y: 40 },
            scroll: 0,
        }));
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 5, y: 5 }, button_h: 0 });
        assert!(!out.iter().any(|c| matches!(c, Command::SetCursorShape { .. })));
    }

    #[test]
    fn a_long_armed_wheel_warns_once() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 500, 200);
        let over = PhysPoint { x: 150, y: 200 };
        let mut warnings = 0;
        for _ in 0..(ARM_WARN_TICKS + 5) {
            let out = c.handle(Event::Tick { cursor: over, button_h: 0 });
            warnings += out
                .iter()
                .filter(|cmd| **cmd == Command::WarnScrollCaptured { seconds: 5 })
                .count();
        }
        assert_eq!(1, warnings);
    }

    // -- movement gate --

    #[test]
    fn the_first_move_always_dispatches() {
        let mut c = test_controller(cfg());
        let out = c.handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } });
        assert_eq!(
            out,
            vec![Command::RequestLookup {
                id: RequestId(1),
                point: PhysPoint { x: 10, y: 10 },
                popup: None,
                session: test_session(),
            }]
        );
    }

    #[test]
    fn the_gate_is_exclusive_at_its_boundary() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 0, y: 0 } });
        // Four physical pixels do not pass the gate.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 4, y: 4 } })
            .is_empty());
        // Five physical pixels pass the gate.
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 5, y: 0 },
                popup: None,
                session: test_session(),
            }],
            c.handle(Event::CursorMoved { pos: PhysPoint { x: 5, y: 0 } })
        );
    }

    #[test]
    fn a_rejected_move_never_becomes_the_new_reference() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 0, y: 0 } });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 3, y: 0 } });
        // The gate still measures from x = 0.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 4, y: 0 } })
            .is_empty());
    }

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

    /// The press over the popup runs a lookup with the popup as the mask.
    #[test]
    fn press_mode_asks_again_at_the_same_point_even_over_the_popup() {
        let mut c = test_controller(press_cfg());
        let over_popup = PhysPoint { x: POPUP.x + 10, y: POPUP.y + 10 };
        pressed_and_shown(&mut c, over_popup);
        assert_eq!(
            vec![Command::RequestLookup { id: RequestId(2), point: over_popup, popup: Some(POPUP), session: test_session() }],
            bind_down(&mut c, "press", TriggerMode::Press, over_popup)
        );
        bind_up(&mut c, "press");
        let out = c.handle(Event::LookupResult { id: RequestId(2), outcome: LookupOutcome::Hide });
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
    }

    #[test]
    fn press_mode_ignores_the_key_release() {
        let mut c = test_controller(press_cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        bind_down(&mut c, "press", TriggerMode::Press, pos);
        let id = c.latest;
        c.handle(ready_id(id, "\u{732B}", ANCHOR));
        c.handle(placed(POPUP, 200, 200));
        assert!(bind_up(&mut c, "press").is_empty());
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

    /// A press while the popup is still placing waits for the rectangle.
    #[test]
    fn a_press_during_placement_waits_for_the_rectangle() {
        let mut c = test_controller(press_cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        bind_down(&mut c, "press", TriggerMode::Press, pos);
        bind_up(&mut c, "press");
        c.handle(ready_id(RequestId(1), "\u{732B}", ANCHOR));
        assert!(bind_down(&mut c, "press", TriggerMode::Press, pos).is_empty());
        bind_up(&mut c, "press");
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(
            out.contains(&Command::RequestLookup { id: RequestId(2), point: pos, popup: Some(POPUP), session: test_session() }),
            "{out:?}"
        );
    }

    #[test]
    fn hold_mode_ignores_moves_until_the_key_is_down() {
        let mut c = test_controller(hold_cfg());
        let pos = PhysPoint { x: 10, y: 10 };
        assert!(c.handle(Event::CursorMoved { pos }).is_empty());
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(1),
                point: pos,
                popup: None,
                session: test_session(),
            }],
            bind_down(&mut c, "hold", TriggerMode::HoldKey, pos)
        );
        assert!(c.handle(Event::CursorMoved { pos }).is_empty());
    }

    #[test]
    fn the_key_coming_up_retracts_the_popup_in_hold_mode() {
        let mut c = test_controller(hold_cfg());
        bind_down(&mut c, "hold", TriggerMode::HoldKey, PhysPoint { x: 110, y: 110 });
        shown(&mut c);
        let out = bind_up(&mut c, "hold");
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
    }

    #[test]
    fn the_key_coming_up_kills_the_answer_still_in_flight() {
        let mut c = test_controller(hold_cfg());
        bind_down(&mut c, "hold", TriggerMode::HoldKey, PhysPoint { x: 110, y: 110 });
        let stale = c.latest;
        bind_up(&mut c, "hold");
        let out = c.handle(Event::LookupResult {
            id: stale,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation_of("\u{732B}")),
                anchor: ANCHOR,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        assert!(out.is_empty());
        assert!(!c.is_shown());
    }

    #[test]
    fn live_mode_never_retracts_on_bind_release() {
        let mut c = test_controller(cfg());
        let pos = PhysPoint { x: 110, y: 110 };
        bind_down(&mut c, "live", TriggerMode::Live, pos);
        shown(&mut c);
        assert!(bind_up(&mut c, "live").is_empty());
        assert!(c.is_shown());
    }

    #[test]
    fn a_second_press_at_the_same_point_looks_up_again_without_cursor_motion() {
        let mut c = test_controller(press_cfg());
        let point = PhysPoint { x: 110, y: 110 };
        pressed_and_shown(&mut c, point);
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point,
                popup: Some(POPUP),
                session: test_session(),
            }],
            bind_down(&mut c, "press", TriggerMode::Press, point)
        );
    }
    #[test]
    fn a_press_bind_pauses_live_until_the_chain_ends() {
        let mut c = test_controller(cfg());
        let press = PhysPoint { x: 110, y: 110 };
        bind_down(&mut c, "press", TriggerMode::Press, press);
        bind_up(&mut c, "press");
        assert!(c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } }).is_empty());

        c.handle(Event::DismissRequested);
        let commands = c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::RequestLookup { point, session, .. }
                if *point == PhysPoint { x: 900, y: 900 } && session == &test_session()
        )));
    }

    #[test]
    fn a_displaced_bind_release_does_not_end_the_new_chain() {
        let mut c = test_controller(press_cfg());
        bind_down(&mut c, "old", TriggerMode::HoldKey, PhysPoint { x: 110, y: 110 });
        let new_request = bind_down(&mut c, "new", TriggerMode::Toggle, PhysPoint { x: 120, y: 120 })
            .into_iter()
            .find_map(|command| match command {
                Command::RequestLookup { id, .. } => Some(id),
                _ => None,
            })
            .expect("the new bind starts a lookup");
        assert_eq!(c.active_bind_id(), Some("new"));
        assert_eq!(TriggerMode::Toggle, c.trigger_mode());
        assert!(bind_up(&mut c, "old").is_empty());
        assert_eq!(c.active_bind_id(), Some("new"));
        assert_eq!(TriggerMode::Toggle, c.trigger_mode());
        assert_eq!(c.latest_lookup, new_request);
    }

    #[test]
    fn a_new_bind_replaces_same_content_and_clears_click_history() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("犬".into())),
        );
        let drill_down_id = c.latest_lookup;
        c.handle(Event::LookupResult {
            id: drill_down_id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        c.handle(placed(POPUP, 200, 200));
        c.surface.as_mut().expect("child popup").selection.card_mut(0).replace(SelRange {
            start: TextAddr { entry: 0, addr: crate::select::DocAddr::START },
            end: TextAddr { entry: 0, addr: crate::select::DocAddr::END },
        });
        assert!(c.has_history());

        let request = bind_down(
            &mut c,
            "replacement",
            TriggerMode::Press,
            PhysPoint { x: 110, y: 110 },
        )
        .into_iter()
        .find_map(|command| match command {
            Command::RequestLookup { id, .. } => Some(id),
            _ => None,
        })
        .expect("the new bind starts a lookup");
        bind_up(&mut c, "replacement");
        let commands = c.handle(ready_id(request, "犬", ANCHOR));
        assert!(commands.iter().any(|command| matches!(command, Command::ShowPopup { .. })));
        assert!(!c.has_history());
        assert!(c.surface.as_ref().expect("replacement popup").history.is_empty());
        assert!(c.selection().expect("new selection").card(0).is_none_or(CardSelection::is_empty));
    }


    #[test]
    fn the_char_freeze_applies_only_in_live_mode() {
        assert!(per_char_freeze(true, TriggerMode::Live));
        assert!(!per_char_freeze(true, TriggerMode::HoldKey));
        assert!(!per_char_freeze(true, TriggerMode::HoldShift));
        assert!(!per_char_freeze(false, TriggerMode::Live));
    }


    // -- the sticky region --

    #[test]
    fn a_move_inside_the_hold_resolves_nothing() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        // This point is inside the anchor hold.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 112, y: 105 } })
            .is_empty());
    }

    #[test]
    fn a_move_onto_the_popup_resolves_nothing() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } })
            .is_empty());
    }

    #[test]
    fn leaving_the_sticky_region_resolves_again() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        let away = PhysPoint { x: 900, y: 900 };
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: away,
                // The request carries the shown popup rectangle for the grab mask.
                popup: Some(POPUP),
                session: test_session(),
            }],
            c.handle(Event::CursorMoved { pos: away })
        );
    }

    #[test]
    fn the_button_strip_holds_the_cursor_too() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let below = PhysPoint { x: 150, y: POPUP.y + POPUP.h + 10 };
        // Without the button, the point is not in the sticky region.
        c.handle(Event::Tick { cursor: below, button_h: 0 });
        assert!(!c.handle(Event::CursorMoved { pos: below }).is_empty());
        // With the button, the point is in the sticky region.
        c.handle(Event::Tick { cursor: below, button_h: 40 });
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 151, y: below.y } })
            .is_empty());
    }

    // -- dwell re-check --

    /// This test covers two separate rules (ARCHITECTURE.md#hover-cadence).
    /// The sticky region suppresses cursor moves, but the dwell re-check still
    /// sends a lookup.
    #[test]
    fn a_dwell_re_asks_the_question_the_popup_answers() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        assert!(c.dwell_armed(), "a placed popup is what a dwell watches");
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 110, y: 110 },
                // A live grab masks the core popup from its OCR input.
                popup: Some(POPUP),
                session: test_session(),
            }],
            c.handle(Event::DwellElapsed)
        );
    }

    /// A cursor move onto the popup must not change the dwell question.
    /// The mask removes the popup text, so the re-check hides the popup that
    /// the dwell watch monitors.
    #[test]
    fn a_dwell_asks_where_the_hover_was_not_where_the_cursor_drifted() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        // The movement gate accepts this point, but the sticky region suppresses its lookup.
        assert!(c.handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } }).is_empty());
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 110, y: 110 },
                popup: Some(POPUP),
                session: test_session(),
            }],
            c.handle(Event::DwellElapsed)
        );
    }

    /// An idle cursor over empty screen needs no dwell watch.
    /// No popup means no re-check and no watch for the platform bin to arm.
    #[test]
    fn nothing_shown_is_never_re_checked() {
        let mut c = test_controller(cfg());
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
        // A hover without an answer is not a shown popup.
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// An unplaced popup is not shown. The mask needs its rectangle, so the
    /// re-check must wait for `PopupPlaced`.
    #[test]
    fn a_popup_awaiting_its_rect_is_never_re_checked() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// Hold mode has no dwell re-check because its grab cannot change.
    #[test]
    fn trigger_mode_has_no_dwell_re_check() {
        let mut c = test_controller(hold_cfg());
        bind_down(&mut c, "hold", TriggerMode::HoldKey, PhysPoint { x: 110, y: 110 });
        shown(&mut c);
        assert!(c.popup().is_some(), "the hold bind popup is on screen");
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// A latched Toggle bind uses live grabs until the bind turns off.
    #[test]
    fn a_latched_toggle_keeps_the_dwell_re_check() {
        let mut c = test_controller(press_cfg());
        assert!(!c.dwell_armed());
        bind_down(&mut c, "toggle", TriggerMode::Toggle, PhysPoint { x: 110, y: 110 });
        shown(&mut c);
        assert!(c.dwell_armed(), "a latched toggle watches the screen");
        let pos = PhysPoint { x: 110, y: 110 };
        assert_eq!(
            vec![Command::RequestLookup { id: RequestId(2), point: pos, popup: Some(POPUP), session: test_session() }],
            c.handle(Event::DwellElapsed)
        );
        bind_down(&mut c, "toggle", TriggerMode::Toggle, pos);
        assert!(!c.dwell_armed(), "toggle-off ends the watch");
    }

    /// A drill-down is not screen content. A dialogue behind it must not change
    /// the history stack that the user opened.
    #[test]
    fn a_drill_down_is_never_re_checked() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("\u{732B}".into())),
        );
        c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("\u{5B57}"))),
        });
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
        // Back returns to the hover card, so the dwell watch starts again.
        c.handle(Event::BackRequested);
        c.handle(placed(POPUP, 200, 200));
        assert!(c.dwell_armed());
    }

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
    fn a_ready_answer_asks_for_a_placement_first() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let out = c.handle(ready("\u{732B}", ANCHOR));
        assert_eq!(
            out,
            vec![Command::ShowPopup {
                presentation: Box::new(presentation_of("\u{732B}")),
                anchor: ANCHOR,
                scroll: 0,
                show_back: false,
            }]
        );
        assert!(c.is_shown());
        // The rectangle is unknown until placement completes.
        assert!(c.popup().is_none());
        let out = c.handle(placed(POPUP, 200, 200));
        assert_eq!(
            out,
            vec![
                Command::DiscardScroll,
                Command::ShowScanOverlay { rects: Vec::new() },
                Command::SyncAnkiButton,
            ]
        );
        assert_eq!(POPUP, c.popup().expect("placed").popup);
    }

    #[test]
    fn an_equal_card_at_a_jittered_anchor_is_never_reshown() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = cursor_lookup_request(&mut c, PhysPoint { x: 900, y: 900 });
        let jittered = PhysRect { x: ANCHOR.x + ANCHOR_JITTER_PX, ..ANCHOR };
        assert!(c
            .handle(Event::LookupResult {
                id,
                outcome: LookupOutcome::Ready {
                    presentation: Box::new(presentation_of("\u{732B}")),
                    anchor: jittered,
                    orientation: Orientation::Horizontal,
                    matched: None,
                    scan: Vec::new(),
                },
            })
            .is_empty());
    }

    #[test]
    fn the_same_card_past_the_jitter_is_placed_again() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = cursor_lookup_request(&mut c, PhysPoint { x: 900, y: 900 });
        let moved = PhysRect { x: ANCHOR.x + ANCHOR_JITTER_PX + 1, ..ANCHOR };
        let out = c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation_of("\u{732B}")),
                anchor: moved,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        assert!(out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })));
    }

    #[test]
    fn a_move_during_placement_waits_for_the_rect() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        // This point lands on the popup.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } })
            .is_empty());
        let out = c.handle(placed(POPUP, 200, 200));
        // The Controller holds this move, then finds that the popup covers the point.
        assert!(!out.iter().any(|cmd| matches!(cmd, Command::RequestLookup { .. })));
    }

    #[test]
    fn a_move_off_the_placed_rect_resolves_after_the_round_trip() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(out.contains(&Command::RequestLookup {
            id: RequestId(2),
            point: PhysPoint { x: 900, y: 900 },
            popup: Some(POPUP),
            session: test_session(),
        }));
    }

    #[test]
    fn clicks_and_wheel_do_nothing_while_the_rect_is_unknown() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(c.handle(Event::Scrolled { notches: -3 }).is_empty());
        assert!(click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::Back),
        )
        .is_empty());
        assert!(c.handle(Event::BackRequested).is_empty());
    }

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

    #[test]
    fn a_failed_press_root_retires_its_chain_and_resumes_live_lookup() {
        let mut c = test_controller(cfg());
        let session = c.future_session.clone();
        let down = c.handle(Event::LookupBindDown {
            bind_id: "press".into(),
            mode: TriggerMode::Press,
            session: session.clone(),
            pos: PhysPoint { x: 110, y: 110 },
        });
        let id = down
            .iter()
            .find_map(|command| match command {
                Command::RequestLookup { id, session: requested, .. } => {
                    assert_eq!(requested, &session);
                    Some(*id)
                }
                _ => None,
            })
            .expect("the Press bind starts a lookup");
        bind_up(&mut c, "press");
        c.handle(ready_id(id, "猫", ANCHOR));
        assert_eq!(TriggerMode::Press, c.trigger_mode());

        c.handle(Event::PopupPlaceFailed);

        assert!(c.chain_bind.is_none());
        assert!(c.pending_cursor.is_none());
        assert_eq!(TriggerMode::Live, c.trigger_mode());
        let later = PhysPoint { x: 900, y: 900 };
        assert!(c.handle(Event::CursorMoved { pos: later }).iter().any(|command| {
            matches!(command, Command::RequestLookup { point, session: requested, .. }
                if *point == later && requested == &session)
        }));
    }

    #[test]
    fn a_failed_root_placement_discards_a_cursor_waiting_for_placement() {
        let mut c = test_controller(cfg());
        let first = PhysPoint { x: 110, y: 110 };
        c.handle(Event::CursorMoved { pos: first });
        let id = c.latest_lookup;
        c.handle(ready_id(id, "猫", ANCHOR));

        let stale = PhysPoint { x: 900, y: 900 };
        assert!(c.handle(Event::CursorMoved { pos: stale }).is_empty());
        assert!(c.pending_cursor.is_some());
        c.handle(Event::PopupPlaceFailed);
        assert!(c.pending_cursor.is_none());

        let current = PhysPoint { x: 1200, y: 1200 };
        let request = c
            .handle(Event::CursorMoved { pos: current })
            .into_iter()
            .find_map(|command| match command {
                Command::RequestLookup { id, point, .. } if point == current => Some(id),
                _ => None,
            })
            .expect("Live lookup resumes at the current cursor");
        c.handle(ready_id(request, "犬", ANCHOR));
        let placed = c.handle(placed(POPUP, 200, 200));
        assert!(!placed.iter().any(|command| matches!(
            command,
            Command::RequestLookup { point, .. } if *point == stale
        )));
    }

    #[test]
    fn a_failed_reshow_leaves_the_popup_where_it_was() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 500, 200);
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::ExpandEntry(0)),
        );
        assert!(c.handle(Event::PopupPlaceFailed).is_empty());
        assert_eq!(POPUP, c.popup().expect("still placed").popup);
    }

    // -- scroll behavior --

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

    #[test]
    fn a_reshow_clamps_a_scroll_the_new_content_cannot_hold() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown_sized(&mut c, 500, 200);
        c.handle(Event::Scrolled { notches: -4 });
        assert_eq!(192, c.popup().expect("placed").scroll);
        // Duplicate markers repaint in place, but the popup measures again.
        c.handle(Event::DupesChecked { generation: 1, dupes: Some(HashSet::new()), session: test_session() });
        c.handle(placed(POPUP, 250, 200));
        assert_eq!(50, c.popup().expect("placed").scroll);
    }

    #[test]
    fn an_expanded_entry_starts_back_at_the_top() {
        let mut c = test_controller(cfg());
        shown_sized(&mut c, 500, 200);
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::ExpandEntry(0)),
        );
        c.handle(placed(POPUP, 250, 200));
        assert_eq!(0, c.popup().expect("placed").scroll);
    }

    // -- click actions --

    #[test]
    fn a_drill_down_click_asks_the_worker() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let out = click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("\u{732B}".into())),
        );
        assert_eq!(
            out,
            vec![Command::RequestDrillDown {
                id: RequestId(2),
                text: "\u{732B}".into(),
                session: test_session(),
            }]
        );
    }

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

    #[test]
    fn a_click_below_the_popup_adds_to_anki_only_when_enabled() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let below = PhysPoint { x: 10, y: POPUP.h + 5 };
        assert!(click(&mut c, below, Button::Primary, None).is_empty());

        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        let out = click(&mut c, below, Button::Primary, None);
        assert!(out.iter().any(|cmd| matches!(cmd, Command::AddNote { .. })));
        // Allow only one add request at a time.
        assert!(c.handle(Event::AddRequested).is_empty());
    }

    #[test]
    fn adding_during_replacement_uses_the_displayed_profile() {
        let mut shown_cfg = cfg();
        shown_cfg.anki_enabled = true;
        shown_cfg.first_dict_only = false;
        shown_cfg.include_dictionary_name = true;
        let shown_session = session_for_controller_cfg(&shown_cfg);

        let mut replacement_cfg = cfg();
        replacement_cfg.anki_enabled = false;
        replacement_cfg.first_dict_only = true;
        replacement_cfg.include_dictionary_name = false;
        let replacement_session = session_for_controller_cfg(&replacement_cfg);

        remember_test_session(&shown_session);
        let mut c = Controller::new(shown_cfg, shown_session.clone());
        let card = Card {
            blocks: vec![
                GlossBlock::parse("First Dictionary", r#"["first definition"]"#),
                GlossBlock::parse("Second Dictionary", r#"["second definition"]"#),
            ],
            ..card("cat")
        };
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));

        let bind = c.handle(Event::LookupBindDown {
            bind_id: "replacement".into(),
            mode: TriggerMode::Press,
            session: replacement_session.clone(),
            pos: PhysPoint { x: 120, y: 120 },
        });
        assert!(bind.iter().any(|command| matches!(
            command,
            Command::RequestLookup { session, .. } if session == &replacement_session
        )));

        let add = c.handle(Event::AddRequested);
        let (session, fields) = add
            .iter()
            .find_map(|command| match command {
                Command::AddNote { session, fields, .. } => Some((session, fields)),
                _ => None,
            })
            .expect("the displayed Anki-enabled profile accepts the add");
        assert_eq!(session, &shown_session);
        assert!(fields["glossary"].contains("First Dictionary"));
        assert!(fields["glossary"].contains("first definition"));
        assert!(fields["glossary"].contains("Second Dictionary"));
        assert!(fields["glossary"].contains("second definition"));
    }

    #[test]
    fn selection_remains_enabled_for_the_displayed_profile_during_replacement() {
        let mut shown_cfg = cfg();
        shown_cfg.anki_enabled = true;
        let shown_session = session_for_controller_cfg(&shown_cfg);
        let mut replacement_cfg = cfg();
        replacement_cfg.anki_enabled = false;
        let replacement_session = session_for_controller_cfg(&replacement_cfg);

        remember_test_session(&shown_session);
        let mut c = Controller::new(shown_cfg, shown_session.clone());
        let card = gloss_card("first", "second");
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));
        c.handle(Event::LookupBindDown {
            bind_id: "replacement".into(),
            mode: TriggerMode::Press,
            session: replacement_session,
            pos: PhysPoint { x: 120, y: 120 },
        });

        let first_path = crate::dict::gloss::NodePath::ROOT.child(0).unwrap();
        let second_path = crate::dict::gloss::NodePath::ROOT.child(1).unwrap();
        let first = TextAddr {
            entry: 0,
            addr: crate::select::DocAddr { path: first_path, byte: 0 },
        };
        let second_end = TextAddr {
            entry: 0,
            addr: crate::select::DocAddr { path: second_path, byte: 1 },
        };
        c.handle(Event::PointerDown {
            local: PhysPoint { x: 0, y: 0 },
            button: Button::Primary,
            hit: None,
            text: Some(first),
        });
        assert!(c
            .handle(Event::PointerMoved {
                local: PhysPoint { x: 5, y: 0 },
                text: Some(second_end),
            })
            .contains(&Command::SetDragging(true)));
        c.handle(Event::PointerUp {
            local: PhysPoint { x: 5, y: 0 },
            button: Button::Primary,
        });

        assert!(c
            .selection()
            .and_then(|selection| selection.card(0))
            .is_some_and(|selection| !selection.is_empty()));
        let add = c.handle(Event::AddRequested);
        let (session, fields) = add
            .iter()
            .find_map(|command| match command {
                Command::AddNote { session, fields, .. } => Some((session, fields)),
                _ => None,
            })
            .expect("the displayed profile accepts a note from its selection");
        assert_eq!(session, &shown_session);
        assert!(fields["glossary"].contains("Test"));
    }

    #[test]
    fn an_add_command_honors_the_dictionary_name_setting() {
        let mut config = cfg();
        config.anki_enabled = true;
        config.include_dictionary_name = false;
        let mut controller = test_controller(config);
        let card = gloss_card("cat", "feline");
        shown_card(&mut controller, presentation_with_card(card.clone(), vec![card]));

        let commands = controller.handle(Event::AddRequested);
        let fields = commands
            .iter()
            .find_map(|command| match command {
                Command::AddNote { fields, .. } => Some(fields),
                _ => None,
            })
            .expect("the add command");
        assert!(!fields["glossary"].contains("Test"));
        assert!(!fields["glossary_html"].contains("Test"));
    }

    #[test]
    fn an_added_note_is_never_added_twice() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        let (id, session) = add_note_request(&c.handle(Event::AddRequested));
        note_added(&mut c, id, "\u{732B}", session);
        c.handle(placed(POPUP, 200, 200));
        assert!(c.handle(Event::AddRequested).is_empty());
        assert!(c.popup().expect("placed").anki.added.contains("\u{732B}"));
    }

    /// A duplicate that the update setting blocks does nothing at all.
    #[test]
    fn a_blocked_duplicate_add_does_nothing() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        a_known_dupe(&mut c);
        let before = c.anki().expect("shown").clone();
        assert!(before.dupes.contains("\u{732B}"));
        assert!(c.handle(Event::AddRequested).is_empty());
        let below = PhysPoint { x: 10, y: POPUP.h + 5 };
        assert!(click(&mut c, below, Button::Primary, None).is_empty());
        assert_eq!(before, *c.anki().expect("shown"));
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
    fn an_add_with_the_probe_asks_for_the_sentence_first() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown(&mut c);
        let out = c.handle(Event::AddRequested);
        assert_eq!(
            out,
            vec![
                Command::RepaintPopup { scroll: 0, show_back: false },
                Command::RequestSentence {
                    id: RequestId(2),
                    anchor: ANCHOR,
                    orientation: Orientation::Horizontal,
                    hide_popup: true,
                    session: test_session(),
                },
                Command::SyncAnkiButton,
            ]
        );
        assert!(c.anki().expect("shown").adding);
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
    #[test]
    fn a_pending_sentence_add_keeps_the_original_card_after_popup_changes() {
        let mut config = cfg();
        config.anki_enabled = true;
        config.sentence_probe = true;
        config.include_dictionary_name = false;
        let card_for_note = |written: &str, gloss: &str| Card {
            written: Some(written.to_string()),
            blocks: vec![GlossBlock::parse(
                "Test",
                &serde_json::json!([gloss]).to_string(),
            )],
            ..card(written)
        };
        let card_a = card_for_note("card A", "A-only gloss");
        let card_b = card_for_note("card B", "B-only gloss");
        let mut c = test_controller(config);
        shown_card(
            &mut c,
            presentation_with_card(card_a.clone(), vec![card_a, card_b]),
        );

        // Select card A before beginning the asynchronous sentence probe.
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::ToggleEntry(0)),
        );
        assert!(
            !c.selection().unwrap().card(0).expect("card A selection").is_empty(),
            "the original note has a selected Entry",
        );
        let request_id = c
            .handle(Event::AddRequested)
            .into_iter()
            .find_map(|command| match command {
                Command::RequestSentence { id, .. } => Some(id),
                _ => None,
            })
            .expect("the sentence request");

        // Expanding card B changes both popup content and the active selection
        // while the sentence result is still pending.
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::ExpandEntry(0)),
        );
        let popup = c.popup().expect("expanded popup");
        assert_eq!(
            popup.presentation.top.as_ref().and_then(|card| card.written.as_deref()),
            Some("card B"),
        );
        assert!(popup.selection.card(0).is_none_or(CardSelection::is_empty));

        let out = c.handle(Event::LookupResult {
            id: request_id,
            outcome: LookupOutcome::Sentence(Some("A sentence".into())),
        });
        let (expr, fields) = match out.as_slice() {
            [Command::AddNote { expr, fields, .. }, Command::SyncAnkiButton] => (expr, fields),
            commands => panic!("expected the original add payload, got {commands:?}"),
        };
        assert_eq!("card A", expr);
        assert_eq!(Some("card A"), fields.get("expression").map(String::as_str));
        assert_eq!(Some("A-only gloss"), fields.get("glossary").map(String::as_str));
        assert_eq!(Some("A-only gloss"), fields.get("glossary_html").map(String::as_str));
        assert_eq!(Some("A sentence"), fields.get("sentence").map(String::as_str));
    }
    #[test]
    fn a_drill_down_during_a_sentence_probe_keeps_the_add_with_card_a() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown_card(&mut c, presentation_of("card A"));

        let sentence_id = c
            .handle(Event::AddRequested)
            .into_iter()
            .find_map(|command| match command {
                Command::RequestSentence { id, .. } => Some(id),
                _ => None,
            })
            .expect("the sentence request");
        assert!(c.anki().expect("card A popup").adding);

        let drill_down_id = match click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("card B".into())),
        )
        .as_slice()
        {
            [Command::RequestDrillDown { id, .. }] => *id,
            commands => panic!("expected the drill-down request, got {commands:?}"),
        };
        let out = c.handle(Event::LookupResult {
            id: drill_down_id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("card B"))),
        });
        assert!(
            out.iter().any(|command| matches!(command, Command::ShowPopup { .. })),
            "the drill-down replaces the visible popup: {out:?}"
        );
        c.handle(placed(POPUP, 200, 200));

        let popup = c.popup().expect("card B popup");
        assert_eq!(
            popup.presentation.top.as_ref().and_then(|card| card.written.as_deref()),
            Some("card B")
        );
        assert!(!popup.anki.adding, "card B must not display card A's pending add");

        let out = c.handle(Event::LookupResult {
            id: sentence_id,
            outcome: LookupOutcome::Sentence(Some("A sentence".into())),
        });
        match out.as_slice() {
            [Command::AddNote { expr, .. }, Command::SyncAnkiButton] => {
                assert_eq!("card A", expr.as_str(), "the sentence probe belongs to card A");
            }
            commands => panic!("expected card A's add command, got {commands:?}"),
        }

        let (id, session) = add_note_request(&out);
        note_added(&mut c, id, "card A", session);
        c.handle(placed(POPUP, 200, 200));
        let popup = c.popup().expect("card B popup after the add");
        assert!(!popup.anki.adding);
        assert!(
            !popup.anki.added.contains("card A"),
            "NoteAdded for card A must not mark card B as added"
        );

        c.handle(Event::BackRequested);
        c.handle(placed(POPUP, 200, 200));
        let popup = c.popup().expect("card A popup after Back");
        assert_eq!(
            popup.presentation.top.as_ref().and_then(|card| card.written.as_deref()),
            Some("card A")
        );
        assert!(!popup.anki.adding);
        assert!(popup.anki.added.contains("card A"));
    }



    #[test]
    fn a_sentence_probe_survives_back_from_the_drill_down_card() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown_card(&mut c, presentation_of("card A"));

        let drill_down_id = match click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("card B".into())),
        )
        .as_slice()
        {
            [Command::RequestDrillDown { id, .. }] => *id,
            commands => panic!("expected the drill-down request, got {commands:?}"),
        };
        c.handle(Event::LookupResult {
            id: drill_down_id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("card B"))),
        });
        c.handle(placed(POPUP, 200, 200));

        let sentence_id = c
            .handle(Event::AddRequested)
            .into_iter()
            .find_map(|command| match command {
                Command::RequestSentence { id, .. } => Some(id),
                _ => None,
            })
            .expect("the sentence request");
        assert!(c.add_in_flight());

        let back = c.handle(Event::BackRequested);
        assert!(
            back.iter().any(|command| matches!(
                command,
                Command::ShowPopup { presentation, .. }
                    if presentation.top.as_ref().and_then(|card| card.written.as_deref())
                        == Some("card A")
            )),
            "Back must restore card A: {back:?}"
        );
        assert!(!c.add_in_flight(), "the restored card A has no pending add");

        let out = c.handle(Event::LookupResult {
            id: sentence_id,
            outcome: LookupOutcome::Sentence(Some("B sentence".into())),
        });
        let (expr, fields) = match out.as_slice() {
            [Command::AddNote { expr, fields, .. }] => (expr, fields),
            commands => panic!("expected card B's isolated add command, got {commands:?}"),
        };
        assert_eq!("card B", expr);
        assert_eq!(Some("B sentence"), fields.get("sentence").map(String::as_str));
    }

    #[test]
    fn a_failed_probe_keeps_the_hover_sentence() {
        let mut presentation = presentation_of("\u{732B}");
        presentation.sentence = Some("hover sentence".into());
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown_card(&mut c, presentation);
        let request_id = match &c.handle(Event::AddRequested)[1] {
            Command::RequestSentence { id, .. } => *id,
            command => panic!("expected a sentence request, got {command:?}"),
        };
        let out = c.handle(Event::LookupResult {
            id: request_id,
            outcome: LookupOutcome::Sentence(None),
        });
        let fields = match out.as_slice() {
            [Command::AddNote { fields, .. }, Command::SyncAnkiButton] => fields,
            commands => panic!("expected the add tail, got {commands:?}"),
        };
        assert_eq!(fields.get("sentence").map(String::as_str), Some("hover sentence"));
    }

    #[test]
    fn a_stale_sentence_result_is_dropped() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown(&mut c);
        let sentence_id = match &c.handle(Event::AddRequested)[1] {
            Command::RequestSentence { id, .. } => *id,
            command => panic!("expected a sentence request, got {command:?}"),
        };

        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let hover_id = c.latest;
        let new_anchor = PhysRect { x: 900, y: 900, w: 20, h: 20 };
        let out = c.handle(ready_id(hover_id, "\u{72AC}", new_anchor));
        assert!(out.iter().any(|command| matches!(command, Command::ShowPopup { .. })));
        assert!(!c.anki().expect("new surface").adding);

        assert!(c
            .handle(Event::LookupResult {
                id: sentence_id,
                outcome: LookupOutcome::Sentence(Some("stale".into())),
            })
            .is_empty());
        assert!(!c.anki().expect("new surface").adding);
    }

    #[test]
    fn a_hover_result_after_a_sentence_request_is_not_stale() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..cfg()
        });
        shown(&mut c);
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let hover_id = c.latest;
        assert!(!c.handle(Event::AddRequested).is_empty());
        assert!(hover_id < c.latest);

        let new_anchor = PhysRect { x: 900, y: 900, w: 20, h: 20 };
        let out = c.handle(ready_id(hover_id, "\u{72AC}", new_anchor));
        assert!(out.iter().any(|command| matches!(command, Command::ShowPopup { .. })));
    }

    /// A platform bin that paints the Anki control inside the popup needs its
    /// state before the first frame. This state exists before the platform knows
    /// the rectangle, so `popup()` returns `None`.
    #[test]
    fn the_anki_state_reads_back_before_the_popup_has_a_rect() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        assert!(c.anki().is_none(), "nothing shown is nothing to paint");

        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let id = c.latest;
        c.handle(ready_id(id, "\u{732B}", ANCHOR));
        assert!(c.popup().is_none(), "no rect has come back yet");
        assert_eq!(c.current_session(), Some(&test_session()));

        let anki = c.anki().expect("a popup on its way still carries Anki state");
        assert!(anki.enabled, "the feature is on");
        assert!(anki.checking, "a fresh popup's first frame is already checking");

        c.handle(placed(POPUP, 200, 200));
        let (id, session) = add_note_request(&c.handle(Event::AddRequested));
        note_added(&mut c, id, "\u{732B}", session);
        assert!(c.anki().expect("the placed popup retains its Anki state").added.contains("\u{732B}"));
    }

    #[test]
    fn a_note_result_without_a_matching_write_is_ignored() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("card B".into())),
        );
        c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("card B"))),
        });
        c.handle(placed(POPUP, 200, 200));

        assert!(c.handle(Event::NoteAdded {
            id: RequestId(999),
            expr: "card B".into(),
            session: test_session(),
            failed: false,
        }).is_empty());
        assert!(!c.anki().expect("card B Anki state").added.contains("card B"));
    }

    #[test]
    fn hidden_click_parent_keeps_session_scoped_anki_results_for_back() {
        let config = ControllerConfig { anki_enabled: true, ..cfg() };
        let mut c = test_controller(config.clone());
        shown(&mut c);
        let generation = c.surface.as_ref().unwrap().generation;
        let analysis_generation = c.surface.as_ref().unwrap().analysis_generation;
        let (write_id, session) = add_note_request(&c.handle(Event::AddRequested));

        let drill_down_id = click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::DrillDown("犬".into())),
        )
        .into_iter()
        .find_map(|command| match command {
            Command::RequestDrillDown { id, .. } => Some(id),
            _ => None,
        })
        .expect("the click drill-down request");
        c.handle(Event::LookupResult {
            id: drill_down_id,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("犬"))),
        });
        c.handle(placed(POPUP, 200, 200));

        c.handle(Event::DupesChecked {
            generation,
            session: session_for_controller_cfg(&config),
            dupes: Some(HashSet::from(["猫".into()])),
        });
        assert!(c.surface.as_ref().unwrap().history[0].anki.checking);
        let words = WordMap::new();
        c.handle(Event::AnalysisReady { generation: analysis_generation, words: words.clone() });
        c.handle(Event::DupesChecked {
            generation,
            session: session.clone(),
            dupes: Some(HashSet::from(["猫".into()])),
        });
        note_added(&mut c, write_id, "猫", session.clone());
        let parent = &c.surface.as_ref().unwrap().history[0];
        assert!(parent.anki.dupes.contains("猫"));
        assert!(parent.anki.added.contains("猫"));
        assert_eq!(parent.analysis, Some((analysis_generation, words)));
        assert!(!parent.anki.adding);

        c.handle(Event::BackRequested);
        assert_eq!(c.surface.as_ref().unwrap().session, session);
        assert!(c.anki().unwrap().dupes.contains("猫"));
        assert!(c.anki().unwrap().added.contains("猫"));
        assert_eq!(c.surface.as_ref().unwrap().analysis_generation, analysis_generation);
    }

    #[test]
    fn a_wrong_session_note_reply_keeps_the_write_pending() {
        let config = ControllerConfig {
            anki_enabled: true,
            overwrite_duplicates: true,
            ..cfg()
        };
        let mut c = test_controller(config.clone());
        shown(&mut c);
        let (id, session) = add_note_request(&c.handle(Event::AddRequested));
        let wrong_session = session_for_controller_cfg(&config);
        assert!(note_written(
            &mut c,
            id,
            "\u{732B}",
            wrong_session,
            NoteWriteStatus::Updated,
        )
        .is_empty());
        assert!(c.anki().expect("popup state").adding);
        assert!(c.pending_writes.contains_key(&id));

        note_written(&mut c, id, "\u{732B}", session, NoteWriteStatus::Updated);
        assert!(c.anki().expect("popup state").updated.contains("\u{732B}"));
        assert!(!c.pending_writes.contains_key(&id));
    }

    #[test]
    fn an_updated_note_blocks_a_second_write_from_the_same_popup() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            overwrite_duplicates: true,
            ..cfg()
        });
        shown(&mut c);
        let commands = c.handle(Event::AddRequested);
        let (id, session) = add_note_request(&commands);
        note_written(
            &mut c,
            id,
            "\u{732B}",
            session,
            NoteWriteStatus::Updated,
        );
        assert!(c.anki().expect("popup state").updated.contains("\u{732B}"));
        assert!(c.handle(Event::AddRequested).is_empty());
    }

    #[test]
    fn a_click_outside_any_region_does_nothing() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        assert!(click(&mut c, PhysPoint { x: 10, y: 10 }, Button::Primary, None).is_empty());
    }

    // -- duplicate checks --

    #[test]
    fn a_new_popup_checks_every_headword_for_dupes() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let mut presentation = presentation_of("\u{732B}");
        presentation.collapsed.push(CollapsedRow {
            written: Some("\u{72AC}".into()),
            reading: None,
            summary: String::new(),
        });
        c.handle(Event::LookupResult {
            id: RequestId(1),
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation),
                anchor: ANCHOR,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(out.contains(&Command::CheckDupes {
            generation: 1,
            exprs: vec!["\u{732B}".into(), "\u{72AC}".into()],
            session: test_session(),
        }));
    }

    #[test]
    fn a_dupe_answer_for_an_older_popup_is_dropped() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        assert!(c
            .handle(Event::DupesChecked { generation: 99, session: test_session(), dupes: None })
            .is_empty());
        let out = c.handle(Event::DupesChecked {
            generation: 1,
            dupes: Some(HashSet::from(["\u{732B}".to_string()])),
            session: test_session(),
        });
        assert!(out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })));
        c.handle(placed(POPUP, 200, 200));
        let view = c.popup().expect("placed");
        assert!(view.anki.connected);
        assert!(!view.anki.checking);
        assert!(view.anki.dupes.contains("\u{732B}"));
    }

    #[test]
    fn a_marker_only_reshow_keeps_analysis_without_a_new_request() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        let card = gloss_card("first", "second");
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));
        let generation = c.surface.as_ref().expect("surface").analysis_generation;
        let words = WordMap::new();
        c.handle(Event::AnalysisReady { generation, words: words.clone() });
        assert_eq!(
            c.surface.as_ref().expect("surface").analysis,
            Some((generation, words.clone()))
        );

        let out = c.handle(Event::DupesChecked { generation, dupes: Some(HashSet::new()), session: test_session() });
        assert!(out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })));
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(!out.iter().any(|cmd| matches!(cmd, Command::RequestAnalysis { .. })));
        assert_eq!(c.surface.as_ref().expect("surface").analysis, Some((generation, words)));
    }

    #[test]
    fn expanding_entry_requests_analysis_once() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        let top = gloss_card("top", "top two");
        let second = gloss_card("second", "second two");
        shown_card(&mut c, presentation_with_card(top.clone(), vec![top, second]));
        let out = click(
            &mut c,
            PhysPoint { x: 10, y: 10 },
            Button::Primary,
            Some(HitAction::ExpandEntry(0)),
        );
        assert!(out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })));

        let out = c.handle(placed(POPUP, 200, 200));
        assert_eq!(
            out.iter()
                .filter(|cmd| matches!(cmd, Command::RequestAnalysis { .. }))
                .count(),
            1
        );
    }

    // -- Worker outcomes --

    #[test]
    fn a_hide_answer_retracts_the_popup() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = cursor_lookup_request(&mut c, PhysPoint { x: 900, y: 900 });
        let out = c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Hide,
        });
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
    }

    #[test]
    fn a_failed_answer_warns_and_retracts() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        let id = cursor_lookup_request(&mut c, PhysPoint { x: 900, y: 900 });
        let out = c.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Failed("ocr died".into()),
        });
        assert_eq!(
            out,
            vec![
                Command::WarnLookupFailed("ocr died".into()),
                Command::HidePopup,
                Command::SetBackArmed(false),
            ]
        );
    }

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

    #[test]
    fn the_lookup_log_stays_quiet_unless_it_is_asked_for() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let out = c.handle(ready("\u{732B}", ANCHOR));
        assert!(!out.iter().any(|cmd| matches!(cmd, Command::LogLookup { .. })));

        let mut c = test_controller(ControllerConfig { log_lookups: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let out = c.handle(ready("\u{732B}", ANCHOR));
        assert_eq!(
            Command::LogLookup { headword: "\u{732B}".into(), match_len: 1 },
            out[0]
        );
    }

    // -- configuration reload --

    #[test]
    fn a_profile_reload_keeps_an_in_flight_lookup_in_its_original_session() {
        let mut c = test_controller(cfg());
        shown(&mut c);
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let pending = c.latest;
        let out = reload(&mut c, ControllerConfig {
            per_character_lookup: true,
            ..cfg()
        });
        assert_eq!(out, vec![Command::RequestReload { id: RequestId(3) }]);
        assert!(c.is_shown());
        assert_eq!(
            c.handle(Event::LookupResult { id: pending, outcome: LookupOutcome::Hide }),
            vec![Command::HidePopup, Command::SetBackArmed(false)]
        );
        assert!(!c.is_shown());
    }

    #[test]
    fn a_profile_reload_does_not_change_the_freeze_rect_of_a_shown_popup() {
        let mut c = test_controller(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(presentation_of("\u{5BBF}\u{820E}")),
                anchor: ANCHOR,
                orientation: Orientation::Horizontal,
                // The matched span contains two glyphs.
                matched: Some(PhysRect { x: 100, y: 100, w: 40, h: 20 }),
                scan: Vec::new(),
            },
        });
        c.handle(placed(POPUP, 200, 200));
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        // The second glyph is inside the span hold.
        let second = PhysPoint { x: 132, y: 105 };
        assert!(c.handle(Event::CursorMoved { pos: second }).is_empty());

        reload(&mut c, ControllerConfig {
            per_character_lookup: true,
            ..cfg()
        });
        assert!(!c.cfg.per_character_lookup);
        // The current profile still freezes the complete matched span.
        let third = PhysPoint { x: 138, y: 105 };
        assert!(c.handle(Event::CursorMoved { pos: third }).is_empty());
    }

    // -- tray and quit --

    #[test]
    fn the_tray_opens_settings_and_quits() {
        let mut c = test_controller(cfg());
        assert_eq!(
            vec![Command::OpenSettings],
            c.handle(Event::TrayAction(TrayAction::OpenSettings))
        );
        assert_eq!(vec![Command::Exit], c.handle(Event::TrayAction(TrayAction::Quit)));
        assert_eq!(vec![Command::Exit], c.handle(Event::Quit));
    }

    // -- hold geometry --

    #[test]
    fn the_hold_covers_the_vertical_slack_hit_scan_allows() {
        let anchor = PhysRect { x: 100, y: 100, w: 20, h: 20 };
        let hold = hold_region(anchor, Some(PhysRect { x: 100, y: 100, w: 60, h: 20 }),
                               Orientation::Horizontal);
        assert_eq!(90, hold.y);
        assert_eq!(40, hold.h);
    }

    #[test]
    fn the_hold_never_widens_along_the_reading_axis() {
        let anchor = PhysRect { x: 100, y: 100, w: 20, h: 20 };
        let matched = PhysRect { x: 100, y: 100, w: 60, h: 20 };
        let hold = hold_region(anchor, Some(matched), Orientation::Horizontal);
        assert_eq!(matched.x, hold.x);
        assert_eq!(matched.w, hold.w);
    }

    #[test]
    fn the_hold_mirrors_for_vertical_text() {
        let anchor = PhysRect { x: 100, y: 100, w: 20, h: 20 };
        let matched = PhysRect { x: 100, y: 100, w: 20, h: 60 };
        let hold = hold_region(anchor, Some(matched), Orientation::Vertical);
        assert_eq!(90, hold.x);
        assert_eq!(40, hold.w);
        assert_eq!(matched.y, hold.y);
        assert_eq!(matched.h, hold.h);
    }

    #[test]
    fn the_hold_without_a_match_still_carries_its_slack() {
        let anchor = PhysRect { x: 100, y: 100, w: 20, h: 20 };
        let hold = hold_region(anchor, None, Orientation::Horizontal);
        assert_eq!(anchor.x, hold.x);
        assert_eq!(anchor.w, hold.w);
        assert_eq!(40, hold.h);
    }

    #[test]
    fn the_char_hold_ignores_the_matched_span() {
        let anchor = PhysRect { x: 100, y: 100, w: 20, h: 20 };
        let matched = PhysRect { x: 100, y: 100, w: 60, h: 20 };
        let HoldRects { hold, hold_char } =
            hold_regions(anchor, Some(matched), Orientation::Horizontal);
        assert_eq!(60, hold.w);
        assert_eq!(20, hold_char.w);
    }

    /// Verify the hold for one matched word and one popup.
    #[test]
    fn the_hold_region_covers_the_whole_matched_word() {
        let anchor = PhysRect { x: 3010, y: 257, w: 27, h: 26 };
        // The match has four characters.
        let matched = PhysRect { x: 3007, y: 254, w: 120, h: 32 };
        let popup = PhysRect { x: 3007, y: 300, w: 420, h: 300 };

        // Test later glyphs from the same word.
        assert!(in_sticky(PhysPoint { x: 3051, y: 270 }, matched, matched, popup));
        assert!(in_sticky(PhysPoint { x: 3100, y: 270 }, matched, matched, popup));
        // A point past the match triggers a new lookup.
        assert!(!in_sticky(PhysPoint { x: 3200, y: 270 }, matched, matched, popup));
        // The anchor alone does not keep this point in the sticky region.
        assert!(!in_sticky(PhysPoint { x: 3051, y: 270 }, anchor, anchor, popup));
    }

    /// The boundary between the freeze hold and popup reach.
    #[test]
    fn a_char_freeze_still_reaches_the_popup() {
        let anchor = PhysRect { x: 3010, y: 257, w: 27, h: 26 };
        let matched = PhysRect { x: 3007, y: 254, w: 120, h: 32 };
        let HoldRects { hold, hold_char } =
            hold_regions(anchor, Some(matched), Orientation::Horizontal);
        let popup = PhysRect { x: 3007, y: hold.y + hold.h + 40, w: 420, h: 300 };
        let x = anchor.x + anchor.w / 2;
        for y in hold_char.y..(popup.y + popup.h) {
            assert!(
                in_sticky(PhysPoint { x, y }, hold_char, hold, popup),
                "row {y} escaped the sticky region",
            );
        }
    }

    /// The add hotkey also waits for the popup rectangle.
    #[test]
    fn the_add_hotkey_waits_for_the_rect() {
        let mut c = test_controller(ControllerConfig { anki_enabled: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(c.handle(Event::AddRequested).is_empty());
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.handle(Event::AddRequested).is_empty());
    }

    #[test]
    fn a_held_trigger_reads_the_frozen_frame_without_a_hide() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..hold_cfg()
        });
        bind_down(&mut c, "hold", TriggerMode::HoldKey, PhysPoint { x: 110, y: 110 });
        shown(&mut c);
        let out = c.handle(Event::AddRequested);
        let request = out.iter().find_map(|command| match command {
            Command::RequestSentence { anchor, orientation, hide_popup, .. } => {
                Some((*anchor, *orientation, *hide_popup))
            }
            _ => None,
        });
        assert_eq!(request, Some((ANCHOR, Orientation::Horizontal, false)));
    }
    #[test]
    fn a_latched_toggle_probe_hides_the_popup_for_live_capture() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: true,
            ..press_cfg()
        });
        bind_down(&mut c, "toggle", TriggerMode::Toggle, PhysPoint { x: 110, y: 110 });
        shown(&mut c);
        let hide_popup = c
            .handle(Event::AddRequested)
            .into_iter()
            .find_map(|command| match command {
                Command::RequestSentence { hide_popup, .. } => Some(hide_popup),
                _ => None,
            });
        assert_eq!(Some(true), hide_popup);
    }


    #[test]
    fn an_add_without_the_probe_is_unchanged() {
        let mut c = test_controller(ControllerConfig {
            anki_enabled: true,
            sentence_probe: false,
            ..cfg()
        });
        shown(&mut c);
        let out = c.handle(Event::AddRequested);
        assert!(out.iter().any(|command| matches!(command, Command::AddNote { .. })));
        assert!(!out.iter().any(|command| matches!(command, Command::RequestSentence { .. })));
    }

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

    #[test]
    fn a_new_lookup_clears_the_current_selection() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        let card = gloss_card("first", "second");
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));
        let path = crate::dict::gloss::NodePath::ROOT.child(0).unwrap();
        let text = TextAddr {
            entry: 0,
            addr: crate::select::DocAddr { path, byte: 0 },
        };
        c.handle(Event::PointerDown {
            local: PhysPoint { x: 0, y: 0 },
            button: Button::Primary,
            hit: None,
            text: Some(text),
        });
        c.handle(Event::PointerMoved {
            local: PhysPoint { x: 5, y: 0 },
            text: Some(TextAddr {
                entry: 0,
                addr: crate::select::DocAddr { path, byte: 1 },
            }),
        });
        c.handle(Event::PointerUp { local: PhysPoint { x: 5, y: 0 }, button: Button::Primary });
        assert!(!c.selection().unwrap().card(0).unwrap().is_empty());
        let id = cursor_lookup_request(&mut c, PhysPoint { x: 900, y: 900 });
        c.handle(ready_id(id, "犬", ANCHOR));
        assert!(c.selection().unwrap().card(0).is_none_or(CardSelection::is_empty));
    }

    #[test]
    fn expanding_a_collapsed_entry_swaps_its_selection() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        let top = gloss_card("top", "top two");
        let second = gloss_card("second", "second two");
        let presentation = presentation_with_card(top.clone(), vec![top, second]);
        shown_card(&mut c, presentation);
        let path = crate::dict::gloss::NodePath::ROOT.child(0).unwrap();
        let text = TextAddr {
            entry: 0,
            addr: crate::select::DocAddr { path, byte: 0 },
        };
        {
            let surface = c.surface.as_mut().unwrap();
            surface.selection.card_mut(0).replace(SelRange {
                start: text,
                end: TextAddr {
                    entry: 0,
                    addr: crate::select::DocAddr { path, byte: 3 },
                },
            });
        }
        click(&mut c, PhysPoint { x: 10, y: 10 }, Button::Primary, Some(HitAction::ExpandEntry(0)));
        let selections = c.selection().unwrap();
        assert!(selections.card(0).is_none_or(CardSelection::is_empty));
        assert!(!selections.card(1).unwrap().is_empty());
        assert_eq!(
            c.popup().unwrap().presentation.top.as_ref().unwrap().written.as_deref(),
            Some("second"),
        );
    }

    #[test]
    fn stale_analysis_ready_is_ignored() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = test_controller(config);
        shown(&mut c);
        let generation = c.surface.as_ref().unwrap().analysis_generation;
        assert!(c
            .handle(Event::AnalysisReady {
                generation: generation.saturating_sub(1),
                words: WordMap::new(),
            })
            .is_empty());
        assert!(c.surface.as_ref().unwrap().analysis.is_none());
    }

    #[test]
    fn a_selected_note_ignores_first_dictionary_only() {
        let block = |name: &str, gloss: &str| {
            GlossBlock::parse(name, &serde_json::json!([gloss]).to_string())
        };
        let p = Presentation {
            top: Some(Card {
                blocks: vec![block("A", "cat"), block("B", "feline")],
                ..card("猫")
            }),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: None,
            surface: None,
        };
        let mut selection = CardSelection::default();
        selection.replace(SelRange {
            start: TextAddr { entry: 0, addr: crate::select::DocAddr::START },
            end: TextAddr { entry: 1, addr: crate::select::DocAddr::END },
        });
        let (_, fields) = note_payload(&p, true, true, &selection, Separator::Ellipsis);
        assert!(fields["glossary"].contains("cat"));
        assert!(fields["glossary"].contains("feline"));
        assert!(
            fields["glossary"].contains("cat<br><br>\n<b>B</b><br>"),
            "selected Dictionary groups need an HTML break: {}",
            fields["glossary"],
        );
        assert!(fields["glossary_html"].contains("cat"));
        assert!(fields["glossary_html"].contains("feline"));
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

    /// The card bolds the on-screen form, not the headword. A conjugated
    /// verb stays conjugated, and a sentence without the form stays plain.
    #[test]
    fn the_sentence_field_bolds_the_matched_surface() {
        let sentence = "山が崩れたり、低い所に水が入ったりするかもしれません。";
        assert_eq!(
            "山が<b>崩れたり</b>、低い所に水が入ったりするかもしれません。",
            bold_surface(sentence, Some("崩れたり"))
        );
        assert_eq!(sentence, bold_surface(sentence, Some("崩れる")));
        assert_eq!(sentence, bold_surface(sentence, Some("")));
        assert_eq!(sentence, bold_surface(sentence, None));
        // Only the first occurrence is bold.
        assert_eq!("<b>山</b>と山", bold_surface("山と山", Some("山")));

        let p = Presentation {
            top: Some(card("\u{732B}")),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: Some("\u{732B}\u{304C}\u{3044}\u{308B}".into()),
            surface: OcrSurface::new("\u{732B}", 1),
        };
        let (_, fields) =
            note_payload(&p, false, true, &CardSelection::default(), Separator::Ellipsis);
        assert_eq!(
            Some(&"<b>\u{732B}</b>\u{304C}\u{3044}\u{308B}".to_string()),
            fields.get("sentence")
        );
    }
}
