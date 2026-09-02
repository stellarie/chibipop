//! The hover/popup state machine.
//!
//! `Event` in, `Command` out. Plain
//! data, physical pixels, no OS: the
//! platform bin runs its own loop,
//! synthesizes Events and executes
//! Commands.

use std::collections::{HashMap, HashSet};

use crate::config::TriggerMode;
use crate::geom::{in_sticky, PhysPoint, PhysRect, ScanRect};
use crate::present::{self, AnkiPopupState, Presentation};
use crate::text::layout::Orientation;

/// Movement gate, physical px.
const MOVEMENT_GATE_PX: i64 = 4;

/// Not slop: UPSCALE 2 rounds.
const ANCHOR_JITTER_PX: i32 = 4;

/// Pixels per wheel notch.
const SCROLL_STEP_PX: i32 = 48;

/// Armed ticks before warning.
const ARM_WARN_TICKS: u32 = 250;

/// Staleness by id, no sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// What a click on a region does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitAction {
    /// Expand collapsed row `i`.
    ExpandEntry(usize),
    /// Look a term up in the panel.
    ///
    /// A headword's kanji, one
    /// character at a time, and a
    /// glossary cross-reference's
    /// whole `?query=` target.
    DrillDown(String),
    /// Hand an `http`/`https` citation
    /// to the user's browser.
    OpenUrl(String),
    /// Navigate back in history.
    Back,
}

/// One lookup's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum LookupOutcome {
    /// No text, or no hits.
    Hide,
    /// Logged; never fatal.
    Failed(String),
    /// `scan` empty without debug.
    Ready {
        presentation: Box<Presentation>,
        anchor: PhysRect,
        /// Which axis the hold may grow.
        orientation: Orientation,
        /// What the top card matched.
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
    },
    /// Kanji drill-down result.
    DrillDown(Box<Presentation>),
}

/// What the tray menu chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    Quit,
}

/// One input to the Controller.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The dispatch tick: live cursor
    /// plus the Anki button's height
    /// (0 when it is not visible).
    Tick { cursor: PhysPoint, button_h: i32 },
    /// Whole wheel notches, up is +.
    Scrolled { notches: i32 },
    /// Popup-local click, hit-tested
    /// by the bin (it owns the paint).
    Clicked { local: PhysPoint, hit: Option<HitAction> },
    /// Anki button or its hotkey.
    AddRequested,
    /// Back button or Escape.
    BackRequested,
    /// The trigger key went down.
    TriggerDown,
    /// The trigger key came up.
    TriggerUp,
    /// A gate-accepted cursor sample.
    CursorMoved { pos: PhysPoint },
    /// The dwell deadline passed with
    /// the cursor still
    /// (ARCHITECTURE.md#hover-cadence).
    DwellElapsed,
    /// The worker answered.
    LookupResult { id: RequestId, outcome: LookupOutcome },
    /// `ShowPopup` landed here.
    PopupPlaced { rect: PhysRect, content_h: i32, view_h: i32 },
    /// `ShowPopup` could not be done.
    PopupPlaceFailed,
    /// One dupe check's answer;
    /// `None` = AnkiConnect refused.
    DupesChecked { generation: u64, dupes: Option<HashSet<String>> },
    /// One add-note's answer.
    NoteAdded { expr: String, failed: bool },
    /// Settings changed under us.
    ConfigReloaded(Box<ControllerConfig>),
    TrayAction(TrayAction),
    Quit,
}

/// One instruction for the bin.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Hover lookup at this point.
    ///
    /// `popup` is our own popup's on-screen rect while the lookup runs,
    /// or `None` when nothing is shown: what a live grab must mask out of
    /// its own OCR input where the platform cannot exclude the surface
    /// (ARCHITECTURE.md#capture-and-masking). A bin whose platform
    /// already excludes it ignores this.
    RequestLookup { id: RequestId, point: PhysPoint, popup: Option<PhysRect> },
    /// Dictionary-only lookup.
    RequestDrillDown { id: RequestId, text: String },
    /// Push settings to the worker.
    RequestReload { id: RequestId },
    /// Measure, place, show, paint;
    /// answer with `PopupPlaced`.
    ShowPopup {
        presentation: Box<Presentation>,
        anchor: PhysRect,
        scroll: i32,
        show_back: bool,
    },
    /// Repaint in place, same rect.
    RepaintPopup { scroll: i32, show_back: bool },
    /// Popup, overlay and button.
    HidePopup,
    ShowScanOverlay { rects: Vec<ScanRect> },
    /// Place/paint/hide the button.
    SyncAnkiButton,
    SetScrollArmed(bool),
    SetClickArmed(bool),
    SetAddArmed(bool),
    SetBackArmed(bool),
    /// Drop banked wheel delta.
    DiscardScroll,
    /// The cursor sits here, popup-
    /// local: the bin hit-tests and
    /// shows the hand when it hits.
    SetCursorShape { local: PhysPoint, scroll: i32 },
    CheckDupes { generation: u64, exprs: Vec<String> },
    AddNote { expr: String, fields: HashMap<String, String> },
    /// Lookup log line, when enabled.
    LogLookup { headword: String, match_len: usize },
    WarnLookupFailed(String),
    WarnScrollCaptured { seconds: u32 },
    /// Hand a glossary citation to
    /// the desktop's own browser.
    ///
    /// `http` or `https` and nothing
    /// else: `layout::link_action`
    /// allow-lists the scheme, since
    /// the URL comes out of a
    /// dictionary file chibipop did
    /// not write.
    OpenUrl(String),
    OpenSettings,
    Exit,
}

/// What the Controller reads from
/// the config; refreshed by reload.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConfig {
    pub trigger_mode: TriggerMode,
    pub per_character_lookup: bool,
    pub scroll_popup: bool,
    pub anki_enabled: bool,
    /// Send only the first dictionary's glossary block to Anki
    /// (upstream 0.9.x "first dict only").
    pub first_dict_only: bool,
    pub summary_chars: usize,
    pub log_lookups: bool,
    /// The bin's dispatch tick, ms.
    pub tick_ms: u32,
}

/// Live mode only, by design.
pub fn per_char_freeze(on: bool, mode: TriggerMode) -> bool {
    on && matches!(mode, TriggerMode::Live)
}

/// The span hold and the char.
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

/// Match one axis, slack other.
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

/// The add-note payload: expr from
/// written (else reading), the first
/// dictionary's blocks only when
/// configured, plus the captured
/// sentence.
///
/// One rule, one place: the state
/// machine and the platform bins
/// (screenshot fields) both call it.
/// Empty expr and no fields = no top
/// card.
pub fn note_payload(
    p: &Presentation,
    first_dict_only: bool,
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
    let blocks_to_send = if first_dict_only {
        &card.blocks[..1.min(card.blocks.len())]
    } else {
        &card.blocks[..]
    };
    let mut fields = crate::anki::fields_from_card(card, blocks_to_send);
    if let Some(sentence) = &p.sentence {
        fields.insert("sentence".to_string(), sentence.clone());
    }
    (expr, fields)
}

/// Saved for back navigation.
#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    presentation: Presentation,
    anki: AnkiPopupState,
}

/// The popup's measured geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    popup: PhysRect,
    /// Natural height, unclamped.
    content_h: i32,
    /// The window's own height.
    view_h: i32,
}

/// Why a `ShowPopup` is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceKind {
    /// A new hover's first popup.
    Fresh,
    /// A drill-down pushed onto it.
    DrillDown,
    /// Same popup, new content.
    Reshow,
}

/// What one popup is showing.
#[derive(Debug, Clone, PartialEq)]
struct Surface {
    /// The hovered glyph's box.
    anchor: PhysRect,
    /// Where the cursor may roam.
    hold: PhysRect,
    /// One character's hold.
    hold_char: PhysRect,
    presentation: Presentation,
    anki: AnkiPopupState,
    /// Drill-down stack.
    history: Vec<HistoryEntry>,
    /// Content offset; 0 is the top.
    scroll: i32,
    /// Stale-dupe guard.
    generation: u64,
    /// `None` until `PopupPlaced`.
    placed: Option<Placed>,
}

/// What the bin may read back.
pub struct PopupView<'a> {
    pub popup: PhysRect,
    /// The hovered glyph's box, for actions that gate on it.
    pub anchor: PhysRect,
    pub scroll: i32,
    pub content_h: i32,
    pub view_h: i32,
    pub presentation: &'a Presentation,
    pub anki: &'a AnkiPopupState,
    pub show_back: bool,
}

/// The hover/popup state machine.
pub struct Controller {
    cfg: ControllerConfig,
    /// The popup, if there is one.
    surface: Option<Surface>,
    /// Overlay rects awaiting a place.
    pending_scan: Vec<ScanRect>,
    /// Why a place is outstanding.
    awaiting: Option<PlaceKind>,
    /// A move seen while unplaced.
    pending_cursor: Option<PhysPoint>,
    /// Last point the gate accepted.
    last_accepted: Option<PhysPoint>,
    /// Point of the newest lookup:
    /// what a dwell re-check re-asks.
    last_dispatch: Option<PhysPoint>,
    /// Trigger key held down.
    trigger_held: bool,
    /// The button's height, last tick.
    button_h: i32,
    /// Consecutive armed ticks.
    armed_ticks: u32,
    next_id: u64,
    latest: RequestId,
    generation: u64,
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
        }
    }

    /// The popup on screen, if its
    /// rect is known.
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
        })
    }

    /// The Anki affordance's state, placed or not.
    ///
    /// [`Controller::popup`] answers only once a rect is known, but a
    /// bin that paints the affordance *into* the popup rather than
    /// beside it needs this state to raster the very first frame.
    pub fn anki(&self) -> Option<&AnkiPopupState> {
        self.surface.as_ref().map(|s| &s.anki)
    }

    /// A popup exists, placed or not.
    pub fn is_shown(&self) -> bool {
        self.surface.is_some()
    }

    /// The one entry point.
    pub fn handle(&mut self, event: Event) -> Vec<Command> {
        match event {
            Event::Tick { cursor, button_h } => self.tick(cursor, button_h),
            Event::Scrolled { notches } => self.scrolled(notches),
            Event::Clicked { local, hit } => self.clicked(local, hit),
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

    /// A fresh id; older answers die.
    fn next_request(&mut self) -> RequestId {
        self.next_id += 1;
        self.latest = RequestId(self.next_id);
        self.latest
    }

    fn tick(&mut self, cursor: PhysPoint, button_h: i32) -> Vec<Command> {
        self.button_h = button_h;
        let placed = self.surface.as_ref().and_then(|s| s.placed);
        // The popup's own rect.
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
        // Wheel-up is positive.
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

    fn clicked(&mut self, local: PhysPoint, hit: Option<HitAction>) -> Vec<Command> {
        let Some(s) = self.surface.as_ref() else { return Vec::new() };
        let Some(p) = s.placed else { return Vec::new() };
        match hit {
            Some(HitAction::ExpandEntry(i)) => {
                let summary = self.cfg.summary_chars;
                let s = self.surface.as_mut().expect("checked above");
                present::swap_top(&mut s.presentation, i, summary);
                s.scroll = 0;
                self.begin_place(PlaceKind::Reshow)
            }
            Some(HitAction::DrillDown(text)) => {
                let id = self.next_request();
                vec![Command::RequestDrillDown { id, text }]
            }
            Some(HitAction::OpenUrl(url)) => vec![Command::OpenUrl(url)],
            Some(HitAction::Back) => self.pop_history(),
            // Below the popup: the button.
            None if local.y >= p.popup.h && self.cfg.anki_enabled => self.start_add(),
            None => Vec::new(),
        }
    }

    fn add_requested(&mut self) -> Vec<Command> {
        if self.surface.is_none() || !self.cfg.anki_enabled {
            return Vec::new();
        }
        self.start_add()
    }

    /// Same guard as the click path.
    fn start_add(&mut self) -> Vec<Command> {
        let Some(s) = self.surface.as_mut() else { return Vec::new() };
        if s.placed.is_none() {
            return Vec::new();
        }
        if s.presentation.top.is_none() {
            return Vec::new();
        }
        let (expr, fields) = note_payload(&s.presentation, self.cfg.first_dict_only);
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
        s.scroll = 0;
        let show_back = !s.history.is_empty();
        let mut out = self.begin_place(PlaceKind::Reshow);
        out.push(Command::SetBackArmed(show_back));
        out
    }

    fn trigger_up(&mut self) -> Vec<Command> {
        self.trigger_held = false;
        // Live mode ignores the key.
        if matches!(self.cfg.trigger_mode, TriggerMode::Live) {
            return Vec::new();
        }
        self.last_accepted = None;
        self.pending_cursor = None;
        // An in-flight hit re-shows it.
        self.next_request();
        if self.surface.take().is_none() {
            return Vec::new();
        }
        self.awaiting = None;
        self.pending_scan.clear();
        vec![Command::HidePopup, Command::SetBackArmed(false)]
    }

    /// Whether a move may count now.
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
        // Rect unknown: decide later.
        if self.surface.as_ref().is_some_and(|s| s.placed.is_none()) {
            self.pending_cursor = Some(pos);
            return Vec::new();
        }
        self.dispatch_hover(pos)
    }

    /// Hold, do not resolve.
    fn dispatch_hover(&mut self, pos: PhysPoint) -> Vec<Command> {
        if self.frozen(pos) {
            return Vec::new();
        }
        let popup = self.shown_popup();
        let id = self.next_request();
        self.last_dispatch = Some(pos);
        vec![Command::RequestLookup { id, point: pos, popup }]
    }

    /// The dwell re-check: re-ask the question the shown popup is the
    /// answer to, at the point that asked it.
    ///
    /// Deliberately past the freeze gate - the whole premise is that
    /// the cursor has *not* moved and the screen under it may have. The
    /// re-grab is damage-gated below the seams, so unchanged pixels
    /// cost no OCR pass and come back as the same presentation, which
    /// `ready` re-presents as nothing; a changed hit updates the popup
    /// and a miss retracts it.
    fn dwell(&mut self) -> Vec<Command> {
        if !self.dwell_armed() {
            return Vec::new();
        }
        let Some(pos) = self.last_dispatch else { return Vec::new() };
        let popup = self.shown_popup();
        let id = self.next_request();
        vec![Command::RequestLookup { id, point: pos, popup }]
    }

    /// Whether a dwell re-check has anything to watch, which is what
    /// the bin arms its dwell watch from: nothing shown must cost no
    /// watch at all (zero idle wakeups).
    ///
    /// Trigger mode has no re-check by construction - its frozen grab
    /// cannot change - and a drill-down is not screen content: a
    /// dialogue advancing behind one must not pop the user's stack.
    pub fn dwell_armed(&self) -> bool {
        matches!(self.cfg.trigger_mode, TriggerMode::Live)
            && self
                .surface
                .as_ref()
                .is_some_and(|s| s.placed.is_some() && s.history.is_empty())
    }

    /// Our own popup, if on screen.
    ///
    /// `None` until `PopupPlaced` says where it landed: an unplaced
    /// surface occupies no pixels yet, so there is nothing to mask.
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
            // Superseded, not an error.
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
            placed: None,
        });
        self.pending_scan = scan;
        out.extend(self.begin_place(PlaceKind::Fresh));
        out
    }

    /// Pushes current, replaces.
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
        s.scroll = 0;
        let mut out = self.begin_place(PlaceKind::DrillDown);
        out.push(Command::SetBackArmed(true));
        out
    }

    /// Measure/place/show, again.
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
        let generation = self.generation.wrapping_add(1);
        let s = self.surface.as_mut().expect("checked above");
        s.placed = Some(Placed { popup: rect, content_h, view_h });
        let span = (content_h - view_h).max(0);
        if s.scroll > span {
            s.scroll = span;
        }

        let mut out = Vec::new();
        let mut exprs: Vec<String> = Vec::new();
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

        if !matches!(kind, PlaceKind::Reshow) && anki_enabled {
            self.generation = generation;
            let s = self.surface.as_mut().expect("checked above");
            s.generation = generation;
            if !exprs.is_empty() {
                out.push(Command::CheckDupes { generation, exprs });
            }
        }

        // Held back while unplaced.
        if let Some(pos) = self.pending_cursor.take() {
            out.extend(self.dispatch_hover(pos));
        }
        out
    }

    fn place_failed(&mut self) -> Vec<Command> {
        let Some(kind) = self.awaiting.take() else { return Vec::new() };
        match kind {
            // Nothing on screen to keep.
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
            // The old popup still stands.
            PlaceKind::DrillDown | PlaceKind::Reshow => {
                let pending = self.pending_cursor.take();
                match pending {
                    Some(pos) => self.dispatch_hover(pos),
                    None => Vec::new(),
                }
            }
        }
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

/// Would it redraw the same?
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

    /// The same, answering one request.
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

    /// A shown, placed popup.
    fn shown(c: &mut Controller) {
        shown_sized(c, 200, 200);
    }

    /// Shown, with this content.
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

    // -- arming --

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
        // view == content: nothing to scroll.
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

    // -- the movement gate --

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
        // 4 px is not past the gate.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 4, y: 4 } })
            .is_empty());
        // 5 px is.
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
        // Still measured from x = 0.
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
        // Inside the anchor's hold.
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
                // The shown popup travels with the request: what the
                // grab must mask out of its own OCR input.
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
        // No button: not sticky.
        c.handle(Event::Tick { cursor: below, button_h: 0 });
        assert!(!c.handle(Event::CursorMoved { pos: below }).is_empty());
        // With one: sticky.
        c.handle(Event::Tick { cursor: below, button_h: 40 });
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 151, y: below.y } })
            .is_empty());
    }

    // -- the dwell re-check --

    /// The divergence in one test: the sticky region silences moves,
    /// and the dwell re-check still asks (ARCHITECTURE.md#hover-cadence).
    #[test]
    fn a_dwell_re_asks_the_question_the_popup_answers() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        assert!(c.dwell_armed(), "a placed popup is what a dwell watches");
        assert_eq!(
            vec![Command::RequestLookup {
                id: RequestId(2),
                point: PhysPoint { x: 110, y: 110 },
                // Live grabs mask our own popup out.
                popup: Some(POPUP),
            }],
            c.handle(Event::DwellElapsed)
        );
    }

    /// The cursor drifting onto the popup must not become the dwell's
    /// question: the mask would blank the popup's own text and the
    /// re-check would retract the popup it is watching.
    #[test]
    fn a_dwell_asks_where_the_hover_was_not_where_the_cursor_drifted() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        // Accepted by the movement gate, silenced by the sticky region.
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

    /// A parked cursor over empty screen is fully idle: no popup, no
    /// re-check, nothing for the bin to keep armed.
    #[test]
    fn nothing_shown_is_never_re_checked() {
        let mut c = Controller::new(cfg());
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
        // A hover with no answer yet is not a shown popup either.
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// Unplaced is not shown: the popup's rect is what the mask needs,
    /// so a re-check before `PopupPlaced` would grab the wrong question.
    #[test]
    fn a_popup_awaiting_its_rect_is_never_re_checked() {
        let mut c = Controller::new(cfg());
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
    }

    /// Trigger mode reads a press-time grab, which cannot change:
    /// there is no dwell re-check in it by construction
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

    /// A drill-down is not screen content: dialogue advancing behind
    /// one must not pop the stack the user navigated into.
    #[test]
    fn a_drill_down_is_never_re_checked() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        c.handle(Event::Clicked {
            local: PhysPoint { x: 10, y: 10 },
            hit: Some(HitAction::DrillDown("\u{732B}".into())),
        });
        c.handle(Event::LookupResult {
            id: c.latest,
            outcome: LookupOutcome::DrillDown(Box::new(presentation_of("\u{5B57}"))),
        });
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.dwell_armed());
        assert!(c.handle(Event::DwellElapsed).is_empty());
        // Back to the hover's own card: watched again.
        c.handle(Event::BackRequested);
        c.handle(placed(POPUP, 200, 200));
        assert!(c.dwell_armed());
    }

    /// The three answers a re-check can have. Same content re-presents
    /// nothing - that is what makes a static screen free above the
    /// grab; a change updates the popup; a miss retracts it.
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

    // -- the placement round-trip --

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
        // Rect unknown until placed.
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
        // Would land on the popup.
        assert!(c
            .handle(Event::CursorMoved { pos: PhysPoint { x: 300, y: 250 } })
            .is_empty());
        let out = c.handle(placed(POPUP, 200, 200));
        // Held back, then held: the
        // rect turned out to cover it.
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
        assert!(c
            .handle(Event::Clicked {
                local: PhysPoint { x: 10, y: 10 },
                hit: Some(HitAction::Back),
            })
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
        c.handle(Event::Scrolled { notches: -1 });
        c.handle(Event::Clicked {
            local: PhysPoint { x: 10, y: 10 },
            hit: Some(HitAction::ExpandEntry(0)),
        });
        assert!(c.handle(Event::PopupPlaceFailed).is_empty());
        assert_eq!(POPUP, c.popup().expect("still placed").popup);
    }

    // -- scrolling --

    #[test]
    fn the_wheel_scrolls_by_whole_notches_and_clamps() {
        let mut c = Controller::new(cfg());
        shown_sized(&mut c, 400, 200);
        assert_eq!(
            vec![Command::RepaintPopup { scroll: SCROLL_STEP_PX, show_back: false }],
            c.handle(Event::Scrolled { notches: -1 })
        );
        // Down past the end clamps.
        c.handle(Event::Scrolled { notches: -100 });
        assert_eq!(200, c.popup().expect("placed").scroll);
        // And 0 is the top.
        c.handle(Event::Scrolled { notches: 100 });
        assert_eq!(0, c.popup().expect("placed").scroll);
        // No movement, no repaint.
        assert!(c.handle(Event::Scrolled { notches: 5 }).is_empty());
    }

    #[test]
    fn a_reshow_clamps_a_scroll_the_new_content_cannot_hold() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown_sized(&mut c, 500, 200);
        c.handle(Event::Scrolled { notches: -4 });
        assert_eq!(192, c.popup().expect("placed").scroll);
        // Dupe markers repaint in place
        // but the popup re-measures.
        c.handle(Event::DupesChecked { generation: 1, dupes: Some(HashSet::new()) });
        c.handle(placed(POPUP, 250, 200));
        assert_eq!(50, c.popup().expect("placed").scroll);
    }

    #[test]
    fn an_expanded_entry_starts_back_at_the_top() {
        let mut c = Controller::new(cfg());
        shown_sized(&mut c, 500, 200);
        c.handle(Event::Scrolled { notches: -4 });
        c.handle(Event::Clicked {
            local: PhysPoint { x: 10, y: 10 },
            hit: Some(HitAction::ExpandEntry(0)),
        });
        c.handle(placed(POPUP, 250, 200));
        assert_eq!(0, c.popup().expect("placed").scroll);
    }

    // -- clicks --

    #[test]
    fn a_drill_down_click_asks_the_worker() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let out = c.handle(Event::Clicked {
            local: PhysPoint { x: 10, y: 10 },
            hit: Some(HitAction::DrillDown("\u{732B}".into())),
        });
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
        c.handle(Event::Clicked {
            local: PhysPoint { x: 10, y: 10 },
            hit: Some(HitAction::DrillDown("\u{732B}".into())),
        });
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
        // Nothing left to pop.
        assert!(c.handle(Event::BackRequested).is_empty());
    }

    #[test]
    fn a_click_below_the_popup_adds_to_anki_only_when_enabled() {
        let mut c = Controller::new(cfg());
        shown(&mut c);
        let below = PhysPoint { x: 10, y: POPUP.h + 5 };
        assert!(c.handle(Event::Clicked { local: below, hit: None }).is_empty());

        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        shown(&mut c);
        let out = c.handle(Event::Clicked { local: below, hit: None });
        assert!(out.iter().any(|cmd| matches!(cmd, Command::AddNote { .. })));
        // One add at a time.
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

    /// A bin that paints the affordance *into* the popup has to know
    /// the state before the first raster - which is before any rect
    /// exists, and therefore before `popup()` answers at all.
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
        assert!(c
            .handle(Event::Clicked { local: PhysPoint { x: 10, y: 10 }, hit: None })
            .is_empty());
    }

    // -- dupe checks --

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

    // -- worker outcomes --

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

    // -- reload --

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
                // A two-glyph span.
                matched: Some(PhysRect { x: 100, y: 100, w: 40, h: 20 }),
                scan: Vec::new(),
            },
        });
        c.handle(placed(POPUP, 200, 200));
        c.handle(Event::Tick { cursor: PhysPoint { x: 110, y: 110 }, button_h: 0 });
        // Second glyph: inside the span hold.
        let second = PhysPoint { x: 132, y: 105 };
        assert!(c.handle(Event::CursorMoved { pos: second }).is_empty());

        c.handle(Event::ConfigReloaded(Box::new(ControllerConfig {
            per_character_lookup: true,
            ..cfg()
        })));
        // The char hold is one glyph wide.
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

    /// One word, one popup.
    #[test]
    fn the_hold_region_covers_the_whole_matched_word() {
        let anchor = PhysRect { x: 3010, y: 257, w: 27, h: 26 };
        // A four-character match.
        let matched = PhysRect { x: 3007, y: 254, w: 120, h: 32 };
        let popup = PhysRect { x: 3007, y: 300, w: 420, h: 300 };

        // Same word, later glyphs.
        assert!(in_sticky(PhysPoint { x: 3051, y: 270 }, matched, matched, popup));
        assert!(in_sticky(PhysPoint { x: 3100, y: 270 }, matched, matched, popup));
        // Past the match: re-resolve.
        assert!(!in_sticky(PhysPoint { x: 3200, y: 270 }, matched, matched, popup));
        // The anchor alone releases.
        assert!(!in_sticky(PhysPoint { x: 3051, y: 270 }, anchor, anchor, popup));
    }

    /// The freeze/reach seam.
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

    /// Rect unknown: adds wait too.
    #[test]
    fn the_add_hotkey_waits_for_the_rect() {
        let mut c = Controller::new(ControllerConfig { anki_enabled: true, ..cfg() });
        c.handle(Event::CursorMoved { pos: PhysPoint { x: 110, y: 110 } });
        c.handle(ready("\u{732B}", ANCHOR));
        assert!(c.handle(Event::AddRequested).is_empty());
        c.handle(placed(POPUP, 200, 200));
        assert!(!c.handle(Event::AddRequested).is_empty());
    }

    /// One rule, three shapes.
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

        // Reading stands in for a missing written form.
        let (expr, fields) = note_payload(&p, false);
        assert_eq!("\u{306D}\u{3053}", expr);
        assert_eq!(Some(&"\u{732B}\u{304C}\u{3044}\u{308B}".to_string()), fields.get("sentence"));
        let both = fields.get("glossary").expect("glossary field");
        assert!(both.contains("cat") && both.contains("feline"), "{both}");

        // First dict only drops the rest.
        let (_, trimmed) = note_payload(&p, true);
        let first = trimmed.get("glossary").expect("glossary field");
        assert!(first.contains("cat") && !first.contains("feline"), "{first}");

        // No top card: nothing to add.
        p.top = None;
        assert_eq!((String::new(), HashMap::new()), note_payload(&p, false));
    }
}
