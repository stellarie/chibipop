//! The Controller is the hover and popup state machine.
//!
//! The platform bin sends `Event` values to the Controller and executes the
//! `Command` values that it returns. The core stores plain data in physical
//! pixels and makes no OS calls.

use std::collections::{HashMap, HashSet};

use crate::analysis::{TextKey, WordMap};
use crate::config::{TriggerMode, TripleClick};
use crate::dict::gloss::{
    extent, leaf_text, leaves, RoleFilter, Separator,
};
use crate::geom::{in_sticky, PhysPoint, PhysRect, ScanRect};
use crate::present::{self, AnkiPopupState, Presentation};
use crate::select::gesture::PressInput;
use crate::select::{
    entries, CardSelection, Coverage, Gesture, GestureEffect, GestureEnv, GestureInput, ItemSource,
    Selections, TextAddr,
};
use crate::text::layout::Orientation;

/// The cursor must move more than this many physical pixels on one axis.
const MOVEMENT_GATE_PX: i64 = 4;

/// This four-pixel limit accounts for the `UPSCALE` value of 2.
const ANCHOR_JITTER_PX: i32 = 4;

/// The scroll distance for one wheel notch, in physical pixels.
const SCROLL_STEP_PX: i32 = 48;

/// The number of armed ticks before the Controller warns the user.
const ARM_WARN_TICKS: u32 = 250;

/// A request becomes stale when a newer `RequestId` exists. The Controller
/// uses request IDs instead of a sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Select or clear the whole glossary of entry `e` of the top Card.
    ///
    /// The Entry-header checkbox emits this action. `e` is the ordinal from
    /// [`crate::select::entries`].
    ToggleEntry(u32),
}

/// A pointer button by role, not by physical code.
///
/// The platform bin maps its button codes here. The Controller decides which
/// role adds to a selection and which role replaces it from the
/// `primary_additive` setting.
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
    /// A dispatch tick with the live cursor and the Anki button height.
    /// The height is zero when the button is not visible.
    Tick { cursor: PhysPoint, button_h: i32 },
    /// The number of complete wheel notches. Up is `+`.
    Scrolled { notches: i32 },
    /// A button press in popup-local coordinates. The platform bin performs
    /// the hit test because it owns the paint. `text` is the gloss address
    /// under the point from [`PopupScene::text_hit`], or `None` when the scene
    /// has no gloss text.
    ///
    /// [`PopupScene::text_hit`]: crate::ui::layout::PopupScene::text_hit
    PointerDown { local: PhysPoint, button: Button, hit: Option<HitAction>, text: Option<TextAddr> },
    /// A pointer move while a button is held. `local` can lie outside the
    /// popup during a drag.
    PointerMoved { local: PhysPoint, text: Option<TextAddr> },
    /// A button release.
    PointerUp { local: PhysPoint, button: Button },
    /// Word boundaries for the top Card from the analysis thread.
    /// A stale `generation` is ignored.
    AnalysisReady { generation: u64, words: WordMap },
    /// A request from the Anki button or its hotkey.
    AddRequested,
    /// A request from the Back button or Escape.
    BackRequested,
    /// The trigger key changed to the down state.
    TriggerDown,
    /// The trigger key changed to the up state.
    TriggerUp,
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
    DupesChecked { generation: u64, dupes: Option<HashSet<String>> },
    /// The result of one add-note request.
    NoteAdded { expr: String, failed: bool },
    /// The platform sent a new Controller configuration.
    ConfigReloaded(Box<ControllerConfig>),
    TrayAction(TrayAction),
    Quit,
}

/// The instruction that the Controller returns to the platform bin.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Request a hover lookup at this physical point.
    ///
    /// `popup` is the core popup's on-screen rectangle while the lookup runs.
    /// It is `None` when no popup is shown.
    /// A live grab must mask this rectangle when the platform bin cannot
    /// exclude the surface from its OCR input
    /// (ARCHITECTURE.md#capture-and-masking).
    /// A platform bin that already excludes the popup ignores this field.
    RequestLookup { id: RequestId, point: PhysPoint, popup: Option<PhysRect> },
    /// Request a lookup from the Dictionary data only.
    RequestDrillDown { id: RequestId, text: String },
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
    CheckDupes { generation: u64, exprs: Vec<String> },
    AddNote { expr: String, fields: HashMap<String, String> },
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
    /// A selection drag started (`true`) or ended (`false`). The platform bin
    /// keeps forwarding pointer moves outside the popup while it is `true`.
    SetDragging(bool),
    OpenSettings,
    Exit,
}

/// Settings that the Controller reads from its configuration. Reload refreshes
/// these settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConfig {
    pub trigger_mode: TriggerMode,
    pub per_character_lookup: bool,
    pub scroll_popup: bool,
    pub anki_enabled: bool,
    /// Send only the first Dictionary's glossary block to Anki.
    /// This matches upstream 0.9.x "first dict only".
    /// An active glossary selection overrides this setting.
    pub first_dict_only: bool,
    pub summary_chars: usize,
    pub log_lookups: bool,
    /// The platform bin dispatch interval, in milliseconds.
    pub tick_ms: u32,
    /// The editorial roles that the popup shows. A selection addresses only
    /// this content.
    pub roles: RoleFilter,
    /// Scroll the popup while a selection drag reaches its edge.
    /// `scroll_popup = false` disables this too.
    pub edge_autoscroll: bool,
    /// The physical primary button adds to a selection. When false, it
    /// replaces the selection and the secondary button adds.
    pub primary_additive: bool,
    /// The separator between disjoint selected fragments in one container.
    pub separator: Separator,
    /// What a triple-click selects.
    pub triple_click: TripleClick,
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
/// A non-empty selection always includes every selected Entry.
/// The caller passes an empty selection only when it wants the whole-card path.
/// Include the captured sentence when it exists.
///
/// The Controller and the platform bins use one rule for screenshot fields.
/// If no top card exists, return an empty `expr` and no fields.
pub fn note_payload(
    p: &Presentation,
    first_dict_only: bool,
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
        crate::anki::fields_from_card(card, blocks_to_send)
    } else {
        crate::anki::fields_from_selection(card, selection, separator)
    };
    if let Some(sentence) = &p.sentence {
        fields.insert("sentence".to_string(), sentence.clone());
    }
    (expr, fields)
}

/// State saved before a drill-down so Back can restore it.
#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    presentation: Presentation,
    anki: AnkiPopupState,
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

/// State for one shown popup.
#[derive(Debug, Clone, PartialEq)]
struct Surface {
    /// The rectangle of the hovered glyph.
    anchor: PhysRect,
    /// The rectangle where the cursor can move without a new lookup.
    hold: PhysRect,
    /// The hold rectangle for one character.
    hold_char: PhysRect,
    presentation: Presentation,
    anki: AnkiPopupState,
    /// The drill-down history stack.
    history: Vec<HistoryEntry>,
    /// The content offset. Zero places the content at the top.
    scroll: i32,
    /// The generation that guards against stale duplicate results.
    generation: u64,
    /// The selected ranges for every card in the presentation.
    selection: Selections,
    /// The current click chain and drag state.
    gesture: Gesture,
    /// The analysis result for the current generation.
    analysis: Option<(u64, WordMap)>,
    /// Whether a Reshow must refresh analysis and gesture state.
    ///
    /// Content changes set this flag before placement so a marker-only Reshow
    /// can retain the current analysis and active gesture.
    analysis_stale: bool,
    /// The generation that the current analysis request uses.
    analysis_generation: u64,
    /// The last deferred link action from a primary pointer press.
    pressed_link: Option<HitAction>,
    /// The last popup-local pointer point during a drag.
    last_drag_point: Option<PhysPoint>,
    /// The placement result stays `None` until the platform sends `PopupPlaced`.
    placed: Option<Placed>,
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
    pub show_back: bool,
    pub selection: &'a Selections,
}

/// The Controller state machine for hover and popup events.
pub struct Controller {
    cfg: ControllerConfig,
    /// The current popup, when one exists.
    surface: Option<Surface>,
    /// Scan overlay rectangles held until popup placement.
    pending_scan: Vec<ScanRect>,
    /// The kind of popup placement that awaits a result.
    awaiting: Option<PlaceKind>,
    /// The latest cursor move received before popup placement.
    pending_cursor: Option<PhysPoint>,
    /// The latest cursor point that passed the movement gate.
    last_accepted: Option<PhysPoint>,
    /// The point of the newest lookup. The dwell re-check uses this point.
    last_dispatch: Option<PhysPoint>,
    /// Whether the trigger key is down.
    trigger_held: bool,
    /// The Anki button height from the latest tick.
    button_h: i32,
    /// The count of consecutive ticks while the scroll action is armed.
    armed_ticks: u32,
    next_id: u64,
    latest: RequestId,
    generation: u64,
    /// The monotonic dispatch tick used by pointer gesture timing.
    clock: u64,
}

impl Controller {
    pub fn new(cfg: ControllerConfig) -> Self {
        Self {
            cfg,
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
            generation: 0,
            clock: 0,
        }
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
            show_back: !s.history.is_empty(),
            selection: &s.selection,
        })
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
            Event::Tick { cursor, button_h } => self.tick(cursor, button_h),
            Event::Scrolled { notches } => self.scrolled(notches),
            Event::PointerDown { local, button, hit, text } => {
                self.pointer_down(local, button, hit, text)
            }
            Event::PointerMoved { local, text } => self.pointer_moved(local, text),
            Event::PointerUp { local, button } => self.pointer_up(local, button),
            Event::AddRequested => self.add_requested(),
            Event::BackRequested => self.pop_history(),
            Event::TriggerDown => {
                self.trigger_held = true;
                Vec::new()
            }
            Event::TriggerUp => self.trigger_up(),
            Event::CursorMoved { pos } => self.cursor_moved(pos),
            Event::DwellElapsed => self.dwell(),
            Event::LookupResult { id, outcome } => self.lookup_result(id, outcome),
            Event::PopupPlaced { rect, content_h, view_h } => {
                self.popup_placed(rect, content_h, view_h)
            }
            Event::PopupPlaceFailed => self.place_failed(),
            Event::DupesChecked { generation, dupes } => self.dupes_checked(generation, dupes),
            Event::AnalysisReady { generation, words } => self.analysis_ready(generation, words),
            Event::NoteAdded { expr, failed } => self.note_added(expr, failed),
            Event::ConfigReloaded(cfg) => {
                self.cfg = *cfg;
                let id = self.next_request();
                vec![Command::RequestReload { id }]
            }
            Event::TrayAction(TrayAction::OpenSettings) => vec![Command::OpenSettings],
            Event::TrayAction(TrayAction::Quit) | Event::Quit => vec![Command::Exit],
        }
    }

    /// Create a new `RequestId`. Older results become stale.
    fn next_request(&mut self) -> RequestId {
        self.next_id += 1;
        self.latest = RequestId(self.next_id);
        self.latest
    }

    fn tick(&mut self, cursor: PhysPoint, button_h: i32) -> Vec<Command> {
        self.clock = self.clock.wrapping_add(1);
        let tick = self.clock;
        self.button_h = button_h;
        let placed = self.surface.as_ref().and_then(|s| s.placed);
        // Use the popup's own rectangle.
        let over_popup = placed.is_some_and(|p| p.popup.contains(cursor));
        let over_popup_or_btn = placed.is_some_and(|p| {
            PhysRect { h: p.popup.h + button_h, ..p.popup }.contains(cursor)
        });
        let armed = self.cfg.scroll_popup
            && over_popup
            && placed.is_some_and(|p| p.content_h > p.view_h);

        let mut out = vec![
            Command::SetScrollArmed(armed),
            Command::SetClickArmed(over_popup_or_btn),
            Command::SetAddArmed(self.surface.is_some() && self.cfg.anki_enabled),
        ];
        if let (Some(p), Some(s)) = (placed, self.surface.as_ref()) {
            if over_popup {
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
        out
    }

    fn has_history(&self) -> bool {
        self.surface.as_ref().is_some_and(|s| !s.history.is_empty())
    }

    fn scrolled(&mut self, notches: i32) -> Vec<Command> {
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
        vec![Command::RepaintPopup {
            scroll: next,
            show_back: !s.history.is_empty(),
        }]
    }

    fn pointer_down(
        &mut self,
        local: PhysPoint,
        button: Button,
        hit: Option<HitAction>,
        text: Option<TextAddr>,
    ) -> Vec<Command> {
        let Some(s) = self.surface.as_ref() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        if !self.cfg.anki_enabled {
            return self.action_on_press(local, hit);
        }

        match hit {
            Some(HitAction::ExpandEntry(index)) => self.expand_entry(index),
            Some(HitAction::Back) => self.pop_history(),
            Some(HitAction::ToggleEntry(entry)) => self.toggle_entry(entry),
            Some(HitAction::DrillDown(query)) if text.is_none() => {
                let id = self.next_request();
                vec![Command::RequestDrillDown { id, text: query }]
            }
            None if text.is_none() && local.y >= p.popup.h => self.start_add(),
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
        if !self.cfg.anki_enabled {
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
        if !self.cfg.anki_enabled {
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

    fn action_on_press(&mut self, local: PhysPoint, hit: Option<HitAction>) -> Vec<Command> {
        let Some(s) = self.surface.as_ref() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        match hit {
            Some(HitAction::ExpandEntry(index)) => self.expand_entry(index),
            Some(HitAction::DrillDown(text)) => {
                let id = self.next_request();
                vec![Command::RequestDrillDown { id, text }]
            }
            Some(HitAction::OpenUrl(url)) => vec![Command::OpenUrl(url)],
            Some(HitAction::Back) => self.pop_history(),
            Some(HitAction::ToggleEntry(entry)) => self.toggle_entry(entry),
            // A click below the popup targets the button.
            None if local.y >= p.popup.h && self.cfg.anki_enabled => self.start_add(),
            None => Vec::new(),
        }
    }

    fn expand_entry(&mut self, index: usize) -> Vec<Command> {
        let summary = self.cfg.summary_chars;
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
        let roles = self.cfg.roles;
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
        vec![Command::RepaintPopup {
            scroll: s.scroll,
            show_back: !s.history.is_empty(),
        }]
    }

    fn run_gesture(&mut self, input: GestureInput) -> Vec<Command> {
        let env = self.gesture_env();
        let roles = self.cfg.roles;
        let effects = {
            let Some(s) = self.surface.as_mut() else { return Vec::new() };
            let Some(card) = s.presentation.top.as_ref() else { return Vec::new() };
            let words = s.analysis.as_ref().map(|(_, words)| words);
            let source = ItemSource::new(card, roles, self.cfg.triple_click, words);
            let selection = s.selection.card_mut(0);
            s.gesture.handle(input, env, &source, selection)
        };
        self.gesture_commands(effects)
    }
    fn gesture_env(&self) -> GestureEnv {
        let tick_ms = u64::from(self.cfg.tick_ms.max(1));
        GestureEnv {
            chain_ticks: 500_u64.div_ceil(tick_ms),
            threshold_px: ANCHOR_JITTER_PX,
            primary_additive: self.cfg.primary_additive,
        }
    }

    fn gesture_commands(&mut self, effects: Vec<GestureEffect>) -> Vec<Command> {
        let mut out = Vec::new();
        for effect in effects {
            match effect {
                GestureEffect::Repaint => {
                    if let Some(s) = self.surface.as_ref() {
                        out.push(Command::RepaintPopup {
                            scroll: s.scroll,
                            show_back: !s.history.is_empty(),
                        });
                    }
                }
                GestureEffect::OpenLink => {
                    let action = self.surface.as_mut().and_then(|s| s.pressed_link.take());
                    match action {
                        Some(HitAction::OpenUrl(url)) => out.push(Command::OpenUrl(url)),
                        Some(HitAction::DrillDown(text)) => {
                            let id = self.next_request();
                            out.push(Command::RequestDrillDown { id, text });
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
        if !self.cfg.anki_enabled || !self.cfg.edge_autoscroll || !self.cfg.scroll_popup {
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
        vec![Command::RepaintPopup {
            scroll: next,
            show_back: !s.history.is_empty(),
        }]
    }

    fn add_requested(&mut self) -> Vec<Command> {
        if self.surface.is_none() || !self.cfg.anki_enabled {
            return Vec::new();
        }
        self.start_add()
    }

    /// Apply the same guard as the pointer path.
    fn start_add(&mut self) -> Vec<Command> {
        let first_dict_only = self.cfg.first_dict_only;
        let separator = self.cfg.separator;
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() {
            return Vec::new();
        }
        if s.presentation.top.is_none() {
            return Vec::new();
        }
        let selection = s.selection.card(0).cloned().unwrap_or_default();
        let (expr, fields) =
            note_payload(&s.presentation, first_dict_only, &selection, separator);
        if s.anki.adding || s.anki.added.contains(&expr) {
            return Vec::new();
        }
        s.anki.adding = true;
        s.anki.failed = false;
        vec![
            Command::RepaintPopup {
                scroll: s.scroll,
                show_back: !s.history.is_empty(),
            },
            Command::AddNote { expr, fields },
            Command::SyncAnkiButton,
        ]
    }

    fn pop_history(&mut self) -> Vec<Command> {
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() {
            return Vec::new();
        }
        let Some(entry) = s.history.pop() else { return Vec::new() };
        s.presentation = entry.presentation;
        s.anki = entry.anki;
        s.selection = Selections::default();
        s.gesture.reset();
        s.analysis = None;
        s.analysis_stale = true;
        s.pressed_link = None;
        s.last_drag_point = None;
        s.scroll = 0;
        let show_back = !s.history.is_empty();
        let mut out = self.begin_place(PlaceKind::Reshow);
        out.push(Command::SetBackArmed(show_back));
        out
    }

    fn trigger_up(&mut self) -> Vec<Command> {
        self.trigger_held = false;
        // Live mode ignores this key event.
        if matches!(self.cfg.trigger_mode, TriggerMode::Live) {
            return Vec::new();
        }
        self.last_accepted = None;
        self.pending_cursor = None;
        // Invalidate any lookup that has not returned.
        self.next_request();
        if self.surface.take().is_none() {
            return Vec::new();
        }
        self.awaiting = None;
        self.pending_scan.clear();
        vec![Command::HidePopup, Command::SetBackArmed(false)]
    }

    /// Whether the current mode accepts a cursor move.
    fn mode_eligible(&self) -> bool {
        match self.cfg.trigger_mode {
            TriggerMode::Live => true,
            _ => self.trigger_held,
        }
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
        if !self.mode_eligible() || !self.gate_open(pos) {
            return Vec::new();
        }
        self.last_accepted = Some(pos);
        // Wait for the popup rectangle before deciding.
        if self.surface.as_ref().is_some_and(|s| s.placed.is_none()) {
            self.pending_cursor = Some(pos);
            return Vec::new();
        }
        self.dispatch_hover(pos)
    }

    /// Keep the current result when the cursor remains in the sticky region.
    fn dispatch_hover(&mut self, pos: PhysPoint) -> Vec<Command> {
        if self.frozen(pos) {
            return Vec::new();
        }
        let popup = self.shown_popup();
        let id = self.next_request();
        self.last_dispatch = Some(pos);
        vec![Command::RequestLookup { id, point: pos, popup }]
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
        let id = self.next_request();
        vec![Command::RequestLookup { id, point: pos, popup }]
    }

    /// Whether the Controller has a dwell re-check to watch. The platform bin
    /// uses this value to arm its dwell watch. No popup means no watch and no
    /// idle wakeups.
    ///
    /// Trigger mode has no re-check because its frozen grab cannot change.
    /// A drill-down is not screen content. A dialogue behind it must not change
    /// the history stack that the user opened.
    pub fn dwell_armed(&self) -> bool {
        matches!(self.cfg.trigger_mode, TriggerMode::Live)
            && self
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

    fn frozen(&self, p: PhysPoint) -> bool {
        let Some(s) = self.surface.as_ref() else { return false };
        let Some(placed) = s.placed else { return false };
        let sticky = PhysRect { h: placed.popup.h + self.button_h, ..placed.popup };
        let freeze = if per_char_freeze(self.cfg.per_character_lookup, self.cfg.trigger_mode) {
            s.hold_char
        } else {
            s.hold
        };
        in_sticky(p, freeze, s.hold, sticky)
    }

    fn lookup_result(&mut self, id: RequestId, outcome: LookupOutcome) -> Vec<Command> {
        if id < self.latest {
            // This result is superseded, not an error.
            return Vec::new();
        }
        match outcome {
            LookupOutcome::Hide => self.hide(),
            LookupOutcome::Failed(msg) => {
                let mut out = vec![Command::WarnLookupFailed(msg)];
                out.extend(self.hide());
                out
            }
            LookupOutcome::DrillDown(presentation) => self.push_drilldown(*presentation),
            LookupOutcome::Ready { presentation, anchor, orientation, matched, scan } => {
                self.ready(*presentation, anchor, orientation, matched, scan)
            }
        }
    }

    fn hide(&mut self) -> Vec<Command> {
        self.surface = None;
        self.awaiting = None;
        self.pending_scan.clear();
        self.pending_cursor = None;
        vec![Command::HidePopup, Command::SetBackArmed(false)]
    }

    fn ready(
        &mut self,
        presentation: Presentation,
        anchor: PhysRect,
        orientation: Orientation,
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
    ) -> Vec<Command> {
        if self
            .surface
            .as_ref()
            .is_some_and(|s| same_content(&s.presentation, s.anchor, &presentation, anchor))
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.cfg.log_lookups {
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
        self.surface = Some(Surface {
            anchor,
            hold,
            hold_char,
            presentation,
            anki: AnkiPopupState::fresh(self.cfg.anki_enabled),
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
            placed: None,
        });

        self.pending_scan = scan;
        out.extend(self.begin_place(PlaceKind::Fresh));
        out
    }

    /// Save the current state, then replace it with the drill-down result.
    fn push_drilldown(&mut self, presentation: Presentation) -> Vec<Command> {
        let anki_enabled = self.cfg.anki_enabled;
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() {
            return Vec::new();
        }
        s.history.push(HistoryEntry {
            presentation: std::mem::replace(&mut s.presentation, presentation),
            anki: std::mem::replace(&mut s.anki, AnkiPopupState::fresh(anki_enabled)),
        });
        s.selection = Selections::default();
        s.gesture.reset();
        s.analysis = None;
        s.analysis_stale = true;
        s.pressed_link = None;
        s.last_drag_point = None;
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
            show_back: !s.history.is_empty(),
        }]
    }

    fn popup_placed(&mut self, rect: PhysRect, content_h: i32, view_h: i32) -> Vec<Command> {
        let Some(kind) = self.awaiting.take() else { return Vec::new() };
        if self.surface.is_none() {
            return Vec::new();
        }
        let anki_enabled = self.cfg.anki_enabled;
        let roles = self.cfg.roles;
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
                out.push(Command::CheckDupes { generation, exprs });
            }
            out.push(Command::RequestAnalysis { generation, texts: analysis_texts });
        }

        // Hold this cursor move until popup placement completes.
        if let Some(pos) = self.pending_cursor.take() {
            out.extend(self.dispatch_hover(pos));
        }
        out
    }

    fn place_failed(&mut self) -> Vec<Command> {
        let Some(kind) = self.awaiting.take() else { return Vec::new() };
        match kind {
            // No popup exists on screen to keep.
            PlaceKind::Fresh => {
                self.surface = None;
                self.pending_scan.clear();
                self.pending_cursor = None;

                vec![
                    Command::HidePopup,
                    Command::SetScrollArmed(false),
                    Command::SetClickArmed(false),
                    Command::SetBackArmed(false),
                ]
            }
            // Keep the old popup on screen.
            PlaceKind::DrillDown | PlaceKind::Reshow => {
                let pending = self.pending_cursor.take();
                match pending {
                    Some(pos) => self.dispatch_hover(pos),
                    None => Vec::new(),
                }
            }
        }
    }

    fn analysis_ready(&mut self, generation: u64, words: WordMap) -> Vec<Command> {
        if !self.cfg.anki_enabled {
            return Vec::new();
        }
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() || s.analysis_generation != generation {
            return Vec::new();
        }
        s.analysis = Some((generation, words));
        self.run_gesture(GestureInput::Analysis)
    }

    fn dupes_checked(
        &mut self,
        generation: u64,
        dupes: Option<HashSet<String>>,
    ) -> Vec<Command> {
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.generation != generation || s.placed.is_none() {
            return Vec::new();
        }
        s.anki.checking = false;
        match dupes {
            Some(dupes) => {
                s.anki.connected = true;
                s.anki.dupes = dupes;
            }
            None => s.anki.connected = false,
        }
        self.begin_place(PlaceKind::Reshow)
    }

    fn note_added(&mut self, expr: String, failed: bool) -> Vec<Command> {
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() {
            return Vec::new();
        }
        s.anki.adding = false;
        if failed {
            s.anki.failed = true;
        } else {
            s.anki.added.insert(expr);
        }
        self.begin_place(PlaceKind::Reshow)
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

    fn cfg() -> ControllerConfig {
        ControllerConfig {
            trigger_mode: TriggerMode::Live,
            per_character_lookup: false,
            scroll_popup: true,
            anki_enabled: false,
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
        }
    }

    fn shown_card(c: &mut Controller, presentation: Presentation) {
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        // view == content: no scroll range exists.
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 150, y: 200 }, button_h: 0 });
        assert!(out.contains(&Command::SetScrollArmed(false)));
        assert!(out.contains(&Command::SetClickArmed(true)));

        let mut c = Controller::new(cfg());
        shown_sized(&mut c, 500, 200);
        let out = c.handle(Event::Tick { cursor: PhysPoint { x: 150, y: 200 }, button_h: 0 });
        assert!(out.contains(&Command::SetScrollArmed(true)));
    }

    #[test]
    fn the_click_arm_covers_the_button_strip_below_the_popup() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let just_below = PhysPoint { x: 150, y: POPUP.y + POPUP.h + 10 };
        let out = c.handle(Event::Tick { cursor: just_below, button_h: 0 });
        assert!(out.contains(&Command::SetClickArmed(false)));
        let out = c.handle(Event::Tick { cursor: just_below, button_h: 40 });
        assert!(out.contains(&Command::SetClickArmed(true)));
    }

    #[test]
    fn the_cursor_shape_is_asked_for_only_over_the_popup() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        let out = c.handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } });
        assert_eq!(
            out,
            vec![Command::RequestLookup {
                id: RequestId(1),
                point: PhysPoint { x: 10, y: 10 },
                popup: None,
            }]
        );
    }

    #[test]
    fn the_gate_is_exclusive_at_its_boundary() {
        let mut c = Controller::new(cfg());
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
            }],
            c.handle(Event::CursorMoved { pos: PhysPoint { x: 5, y: 0 } })
        );
    }

    #[test]
    fn a_rejected_move_never_becomes_the_new_reference() {
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 0, y: 0 } });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 3, y: 0 } });
        // The gate still measures from x = 0.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 4, y: 0 } })
            .is_empty());
    }

    // -- trigger freeze --

    fn hold_cfg() -> ControllerConfig {
        ControllerConfig { trigger_mode: TriggerMode::HoldKey, ..cfg() }
    }

    #[test]
    fn hold_mode_ignores_moves_until_the_key_is_down() {
        let mut c = Controller::new(hold_cfg());
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } })
            .is_empty());
        c.handle(Event::TriggerDown);
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(1),
                point: PhysPoint { x: 10, y: 10 },
                popup: None,
            }],
            c.handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } })
        );
    }

    #[test]
    fn the_key_coming_up_retracts_the_popup_in_hold_mode() {
        let mut c = Controller::new(hold_cfg());
        c.handle(Event::TriggerDown);
        shown(&mut c);
        let out = c.handle(Event::TriggerUp);
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
    }

    #[test]
    fn the_key_coming_up_kills_the_answer_still_in_flight() {
        let mut c = Controller::new(hold_cfg());
        c.handle(Event::TriggerDown);
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let stale = c.latest;
        c.handle(Event::TriggerUp);
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
    fn live_mode_never_retracts_on_the_key() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        assert!(c.handle(Event::TriggerUp).is_empty());
        assert!(c.is_shown());
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        // This point is inside the anchor hold.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 112, y: 105 } })
            .is_empty());
    }

    #[test]
    fn a_move_onto_the_popup_resolves_nothing() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } })
            .is_empty());
    }

    #[test]
    fn leaving_the_sticky_region_resolves_again() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        let away = PhysPoint { x: 900, y: 900 };
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: away,
                // The request carries the shown popup rectangle for the grab mask.
                popup: Some(POPUP),
            }],
            c.handle(Event::CursorMoved { pos: away })
        );
    }

    #[test]
    fn the_button_strip_holds_the_cursor_too() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        assert!(c.dwell_armed(), "a placed popup is what a dwell watches");
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 110, y: 110 },
                // A live grab masks the core popup from its OCR input.
                popup: Some(POPUP),
            }],
            c.handle(Event::DwellElapsed)
        );
    }

    /// A cursor move onto the popup must not change the dwell question.
    /// The mask removes the popup text, so the re-check hides the popup that
    /// the dwell watch monitors.
    #[test]
    fn a_dwell_asks_where_the_hover_was_not_where_the_cursor_drifted() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        // The movement gate accepts this point, but the sticky region suppresses its lookup.
        assert!(c.handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } }).is_empty());
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 110, y: 110 },
                popup: Some(POPUP),
            }],
            c.handle(Event::DwellElapsed)
        );
    }

    /// An idle cursor over empty screen needs no dwell watch.
    /// No popup means no re-check and no watch for the platform bin to arm.
    #[test]
    fn nothing_shown_is_never_re_checked() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// Trigger mode uses a press-time grab that cannot change.
    /// It has no dwell re-check by construction
    /// (ARCHITECTURE.md#hover-cadence).
    #[test]
    fn trigger_mode_has_no_dwell_re_check() {
        let mut c = Controller::new(hold_cfg());
        c.handle(Event::TriggerDown);
        shown(&mut c);
        assert!(c.popup().is_some(), "the hold's popup is on screen");
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// A drill-down is not screen content. A dialogue behind it must not change
    /// the history stack that the user opened.
    #[test]
    fn a_drill_down_is_never_re_checked() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let jittered = PhysRect { x: ANCHOR.x + ANCHOR_JITTER_PX, ..ANCHOR };
        assert!(c
            .handle(Event::LookupResult {
                id: c.latest,
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let moved = PhysRect { x: ANCHOR.x + ANCHOR_JITTER_PX + 1, ..ANCHOR };
        let out = c.handle(Event::LookupResult {
            id: c.latest,
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(out.contains(&Command::RequestLookup {
            id: RequestId(2),
            point: PhysPoint { x: 900, y: 900 },
            popup: Some(POPUP),
        }));
    }

    #[test]
    fn clicks_and_wheel_do_nothing_while_the_rect_is_unknown() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
    fn a_failed_reshow_leaves_the_popup_where_it_was() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown_sized(&mut c, 500, 200);
        c.handle(Event::Scrolled { notches: -4 });
        assert_eq!(192, c.popup().expect("placed").scroll);
        // Duplicate markers repaint in place, but the popup measures again.
        c.handle(Event::DupesChecked { generation: 1, dupes: Some(HashSet::new()) });
        c.handle(placed(POPUP, 250, 200));
        assert_eq!(50, c.popup().expect("placed").scroll);
    }

    #[test]
    fn an_expanded_entry_starts_back_at_the_top() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(cfg());
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
            }]
        );
    }

    #[test]
    fn a_drill_down_answer_pushes_history_and_back_pops_it() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
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

        let out = c.handle(Event::BackRequested);
        assert_eq!(
            out,
            vec![
                Command::ShowPopup {
                    presentation: Box::new(presentation_of("\u{732B}")),
                    anchor: ANCHOR,
                    scroll: 0,
                    show_back: false,
                },
                Command::SetBackArmed(false),
            ]
        );
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.popup().expect("placed").show_back);
        // No history entry remains to remove.
        assert!(c.handle(Event::BackRequested).is_empty());
    }

    #[test]
    fn a_click_below_the_popup_adds_to_anki_only_when_enabled() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let below = PhysPoint { x: 10, y: POPUP.h + 5 };
        assert!(click(&mut c, below, Button::Primary, None).is_empty());

        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        let out = click(&mut c, below, Button::Primary, None);
        assert!(out.iter().any(|cmd| matches!(cmd, Command::AddNote { .. })));
        // Allow only one add request at a time.
        assert!(c.handle(Event::AddRequested).is_empty());
    }

    #[test]
    fn an_added_note_is_never_added_twice() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        c.handle(Event::AddRequested);
        c.handle(Event::NoteAdded { expr: "\u{732B}".into(), failed: false });
        c.handle(placed(POPUP, 200, 200));
        assert!(c.handle(Event::AddRequested).is_empty());
        assert!(c.popup().expect("placed").anki.added.contains("\u{732B}"));
    }

    /// A platform bin that paints the Anki control inside the popup needs its
    /// state before the first frame. This state exists before the platform knows
    /// the rectangle, so `popup()` returns `None`.
    #[test]
    fn the_anki_state_reads_back_before_the_popup_has_a_rect() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        assert!(c.anki().is_none(), "nothing shown is nothing to paint");

        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let id = c.latest;
        c.handle(ready_id(id, "\u{732B}", ANCHOR));
        assert!(c.popup().is_none(), "no rect has come back yet");

        let anki = c.anki().expect("a popup on its way still carries Anki state");
        assert!(anki.enabled, "the feature is on");
        assert!(anki.checking, "a fresh popup's first frame is already checking");

        c.handle(placed(POPUP, 200, 200));
        c.handle(Event::NoteAdded { expr: "\u{732B}".into(), failed: false });
        assert!(
            c.anki().expect("still shown").added.contains("\u{732B}"),
            "and it follows the add lifecycle, rect or no rect"
        );
    }

    #[test]
    fn a_click_outside_any_region_does_nothing() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        assert!(click(&mut c, PhysPoint { x: 10, y: 10 }, Button::Primary, None).is_empty());
    }

    // -- duplicate checks --

    #[test]
    fn a_new_popup_checks_every_headword_for_dupes() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
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
        }));
    }

    #[test]
    fn a_dupe_answer_for_an_older_popup_is_dropped() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        assert!(c
            .handle(Event::DupesChecked { generation: 99, dupes: None })
            .is_empty());
        let out = c.handle(Event::DupesChecked {
            generation: 1,
            dupes: Some(HashSet::from(["\u{732B}".to_string()])),
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
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        let card = gloss_card("first", "second");
        shown_card(&mut c, presentation_with_card(card.clone(), vec![card]));
        let generation = c.surface.as_ref().expect("surface").analysis_generation;
        let words = WordMap::new();
        c.handle(Event::AnalysisReady { generation, words: words.clone() });
        assert_eq!(
            c.surface.as_ref().expect("surface").analysis,
            Some((generation, words.clone()))
        );

        let out = c.handle(Event::DupesChecked { generation, dupes: Some(HashSet::new()) });
        assert!(out.iter().any(|cmd| matches!(cmd, Command::ShowPopup { .. })));
        let out = c.handle(placed(POPUP, 200, 200));
        assert!(!out.iter().any(|cmd| matches!(cmd, Command::RequestAnalysis { .. })));
        assert_eq!(c.surface.as_ref().expect("surface").analysis, Some((generation, words)));
    }

    #[test]
    fn expanding_entry_requests_analysis_once() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
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
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let out = c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::Hide,
        });
        assert_eq!(out, vec![Command::HidePopup, Command::SetBackArmed(false)]);
        assert!(!c.is_shown());
    }

    #[test]
    fn a_failed_answer_warns_and_retracts() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let out = c.handle(Event::LookupResult {
            id: c.latest,
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
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 10, y: 10 } });
        let stale = c.latest;
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 100, y: 100 } });
        assert!(c
            .handle(Event::LookupResult { id: stale, outcome: LookupOutcome::Hide })
            .is_empty());
    }

    #[test]
    fn the_lookup_log_stays_quiet_unless_it_is_asked_for() {
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let out = c.handle(ready("\u{732B}", ANCHOR));
        assert!(!out.iter().any(|cmd| matches!(cmd, Command::LogLookup { .. })));

        let mut c = Controller::new(ControllerConfig { log_lookups: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        let out = c.handle(ready("\u{732B}", ANCHOR));
        assert_eq!(
            Command::LogLookup { headword: "\u{732B}".into(), match_len: 1 },
            out[0]
        );
    }

    // -- configuration reload --

    #[test]
    fn a_reload_kills_the_answer_in_flight_and_keeps_the_popup() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 900, y: 900 } });
        let stale = c.latest;
        let out = c.handle(Event::ConfigReloaded(Box::new(ControllerConfig {
            per_character_lookup: true,
            ..cfg()
        })));
        assert_eq!(out, vec![Command::RequestReload { id: RequestId(3) }]);
        assert!(c.is_shown());
        assert!(c
            .handle(Event::LookupResult { id: stale, outcome: LookupOutcome::Hide })
            .is_empty());
    }

    #[test]
    fn a_reload_swaps_the_freeze_rect_mid_hover() {
        let mut c = Controller::new(cfg());
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

        c.handle(Event::ConfigReloaded(Box::new(ControllerConfig {
            per_character_lookup: true,
            ..cfg()
        })));
        // The character hold covers one glyph.
        let third = PhysPoint { x: 138, y: 105 };
        assert!(!c.handle(Event::CursorMoved { pos: third }).is_empty());
    }

    // -- tray and quit --

    #[test]
    fn the_tray_opens_settings_and_quits() {
        let mut c = Controller::new(cfg());
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
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(c.handle(Event::AddRequested).is_empty());
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.handle(Event::AddRequested).is_empty());
    }

    // -- selection/controller behavior --
    #[test]
    fn a_drag_over_two_glosses_updates_the_public_selection() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = Controller::new(config);
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
        let mut c = Controller::new(config);
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
        c.handle(ready_id(c.latest, "犬", ANCHOR));
        assert!(c.selection().unwrap().card(0).is_none_or(CardSelection::is_empty));
    }

    #[test]
    fn expanding_a_collapsed_entry_swaps_its_selection() {
        let mut config = cfg();
        config.anki_enabled = true;
        let mut c = Controller::new(config);
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
        let mut c = Controller::new(config);
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
        };
        let mut selection = CardSelection::default();
        selection.replace(SelRange {
            start: TextAddr { entry: 0, addr: crate::select::DocAddr::START },
            end: TextAddr { entry: 1, addr: crate::select::DocAddr::END },
        });
        let (_, fields) = note_payload(&p, true, &selection, Separator::Ellipsis);
        assert!(fields["glossary"].contains("cat"));
        assert!(fields["glossary"].contains("feline"));
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
        };

        // The `reading` value replaces the missing `written` value.
        let empty = CardSelection::default();
        let (expr, fields) = note_payload(&p, false, &empty, Separator::Ellipsis);
        assert_eq!("\u{306D}\u{3053}", expr);
        assert_eq!(Some(&"\u{732B}\u{304C}\u{3044}\u{308B}".to_string()), fields.get("sentence"));
        let both = fields.get("glossary").expect("glossary field");
        assert!(both.contains("cat") && both.contains("feline"), "{both}");

        // The first Dictionary excludes all later blocks.
        let (_, trimmed) = note_payload(&p, true, &empty, Separator::Ellipsis);
        let first = trimmed.get("glossary").expect("glossary field");
        assert!(first.contains("cat") && !first.contains("feline"), "{first}");

        // Without a top card, the payload has nothing to add.
        p.top = None;
        assert_eq!(
            (String::new(), HashMap::new()),
            note_payload(&p, false, &empty, Separator::Ellipsis),
        );
    }
}
