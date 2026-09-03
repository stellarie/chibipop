//! The pointer gesture state machine for glossary selection.
//!
//! This module owns timing, click stages, and drag transitions without reading
//! controller state. The controller supplies an [`ItemSource`] for the current
//! Card and applies the returned effects to platform commands.

use crate::analysis::{word_at, WordMap};
use crate::dict::gloss::{extent, grapheme_range, leaf_text, sense_range, DocAddr, NodePath, RoleFilter};
use crate::geom::PhysPoint;
use crate::present::{Card, GlossEntry};

use super::{CardSelection, SelRange, TextAddr};

/// The physical pointer button role that a gesture receives.
///
/// The Controller owns this type so platform events and gestures share one enum.
pub use crate::controller::Button;


/// The resolution stage for a click or drag endpoint.
///
/// Grapheme resolution also supplies the snapping boundary for a drag. The
/// other stages match the progressively larger items in a click chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureStage {
    Grapheme,
    Word,
    Sense,
    Entry,
}

/// The short name is useful in table-driven resolver tests.
pub type Stage = GestureStage;

/// The payload for a pointer press.
///
/// A named payload keeps the gesture state machine call small as press data grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressInput {
    pub addr: Option<TextAddr>,
    pub link: bool,
    pub button: Button,
    pub local: PhysPoint,
    pub tick: u64,
}

/// Input to [`Gesture::handle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureInput {
    Press(PressInput),
    Move {
        addr: Option<TextAddr>,
        local: PhysPoint,
        tick: u64,
    },
    Release {
        button: Button,
        local: PhysPoint,
        tick: u64,
    },
    Tick {
        tick: u64,
    },
    Analysis,
}

/// Values that the controller converts into commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureEffect {
    Repaint,
    OpenLink,
    DragStarted,
    DragEnded,
    NeedWord {
        entry: u32,
        path: NodePath,
        byte: u32,
    },
}

/// Timing and selection settings for one gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureEnv {
    /// The inclusive number of ticks in a click chain.
    pub chain_ticks: u64,
    /// The physical movement that changes a click into a drag.
    pub threshold_px: i32,
    /// Whether the physical primary button adds instead of replacing.
    pub primary_additive: bool,
}

/// Resolves a text address into one selectable item.
///
/// The card constructor keeps this module independent of the Controller. Tests
/// can instead use [`ItemSource::from_fn`] with a small table of ranges.
pub struct ItemSource<'a> {
    source: ItemSourceKind<'a>,
}

enum ItemSourceKind<'a> {
    Card {
        card: &'a Card,
        roles: RoleFilter,
        words: Option<&'a WordMap>,
    },
    Resolver(&'a dyn Fn(GestureStage, TextAddr) -> Option<SelRange>),
}

impl<'a> ItemSource<'a> {
    /// Builds a resolver for one Card and its current analysis result.
    pub fn new(card: &'a Card, roles: RoleFilter, words: Option<&'a WordMap>) -> Self {
        Self {
            source: ItemSourceKind::Card { card, roles, words },
        }
    }

    /// Builds a resolver for state-machine tests or another pure caller.
    pub fn from_fn(
        resolver: &'a dyn Fn(GestureStage, TextAddr) -> Option<SelRange>,
    ) -> Self {
        Self { source: ItemSourceKind::Resolver(resolver) }
    }

    /// Resolves one endpoint at the requested stage.
    pub fn item(&self, stage: GestureStage, addr: TextAddr) -> Option<SelRange> {
        match self.source {
            ItemSourceKind::Resolver(resolver) => resolver(stage, addr),
            ItemSourceKind::Card { card, roles, words } => resolve_card(card, roles, words, stage, addr),
        }
    }
}

fn resolve_card(
    card: &Card,
    roles: RoleFilter,
    words: Option<&WordMap>,
    stage: GestureStage,
    addr: TextAddr,
) -> Option<SelRange> {
    let entry = card_entry(card, addr.entry)?;
    let range = match stage {
        GestureStage::Grapheme => grapheme_range(&entry.doc, addr.addr)?,
        GestureStage::Word => {
            let words = words?;
            let text = leaf_text(&entry.doc, addr.addr.path);
            if text.is_empty() {
                return None;
            }
            let ranges = words.get(&(addr.entry, addr.addr.path)).map(Vec::as_slice).unwrap_or(&[]);
            let bytes = word_at(text, ranges, addr.addr.byte as usize);
            if bytes.start == bytes.end {
                return None;
            }
            crate::dict::gloss::DocRange {
                start: DocAddr { path: addr.addr.path, byte: bytes.start as u32 },
                end: DocAddr { path: addr.addr.path, byte: bytes.end as u32 },
            }
        }
        GestureStage::Sense => sense_range(&entry.doc, roles, addr.addr)?,
        GestureStage::Entry => extent(&entry.doc, roles)?,
    };
    Some(SelRange {
        start: TextAddr { entry: addr.entry, addr: range.start },
        end: TextAddr { entry: addr.entry, addr: range.end },
    })
}

fn card_entry(card: &Card, ordinal: u32) -> Option<&GlossEntry> {
    super::entries(card).find_map(|(entry, value)| (entry == ordinal).then_some(value))
}

/// Pure state for a click chain and an optional drag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gesture {
    stage: u8,
    snapshot: CardSelection,
    anchor: Option<TextAddr>,
    press_point: Option<PhysPoint>,
    active_button: Option<Button>,
    current_link: bool,
    dragging: bool,
    last_press: Option<PressPoint>,
    pending_clear: Option<u64>,
    pending_word: Option<PendingWord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PressPoint {
    local: PhysPoint,
    tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingWord {
    addr: TextAddr,
    button: Button,
}

impl Gesture {
    /// Handles one input and mutates only the supplied Card selection.
    pub fn handle(
        &mut self,
        input: GestureInput,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        match input {
            GestureInput::Press(input) => self.press(input, env, source, selection),
            GestureInput::Move { addr, local, tick } => {
                self.move_pointer(addr, local, tick, env, source, selection)
            }
            GestureInput::Release { button, local, tick } => {
                self.release(button, local, tick, env, selection)
            }
            GestureInput::Tick { tick } => self.tick(tick, selection),
            GestureInput::Analysis => self.analysis(env, source, selection),
        }
    }

    /// Alias that reads naturally at the controller seam.
    pub fn input(
        &mut self,
        input: GestureInput,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        self.handle(input, env, source, selection)
    }

    /// Ends a chain without changing the current selection.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reports whether the pointer is in an active drag.
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    fn press(
        &mut self,
        input: PressInput,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        let PressInput { addr, link, button, local, tick } = input;
        let chained = self.last_press.is_some_and(|previous| {
            tick.saturating_sub(previous.tick) <= env.chain_ticks
                && within(previous.local, local, env.threshold_px)
        });
        let mut effects = if chained {
            Vec::new()
        } else {
            let mut effects = self.expire(tick, selection);
            if self.pending_clear.take().is_some() && !selection.is_empty() {
                selection.clear();
                effects.push(GestureEffect::Repaint);
            }
            effects
        };
        if chained {
            self.stage = self.stage.saturating_add(1).max(2);
        } else {
            self.stage = 1;
            self.snapshot = selection.clone();
        }
        self.anchor = addr;
        self.press_point = Some(local);
        self.active_button = Some(button);
        self.current_link = link;
        self.dragging = false;
        self.last_press = Some(PressPoint { local, tick });
        self.pending_word = None;
        if chained {
            self.pending_clear = None;
        }
        let Some(addr) = addr else {
            self.stage = 1;
            return effects;
        };
        if self.stage >= 2 {
            effects.extend(self.apply_stage(addr, button, env, source, selection));
        }
        effects
    }

    fn move_pointer(
        &mut self,
        addr: Option<TextAddr>,
        local: PhysPoint,
        _tick: u64,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        let Some(button) = self.active_button else { return Vec::new() };
        let Some(press_point) = self.press_point else { return Vec::new() };
        if self.anchor.is_none() {
            return Vec::new();
        }
        if !self.dragging {
            if !beyond(press_point, local, env.threshold_px) {
                return Vec::new();
            }
            self.dragging = true;
            let mut effects = vec![GestureEffect::DragStarted];
            if let Some(item) = self.drag_item(addr, source) {
                self.apply(selection, item, button, env);
                effects.push(GestureEffect::Repaint);
            }
            return effects;
        }

        let Some(item) = self.drag_item(addr, source) else { return Vec::new() };
        self.apply(selection, item, button, env);
        vec![GestureEffect::Repaint]
    }

    fn drag_item(&self, addr: Option<TextAddr>, source: &ItemSource<'_>) -> Option<SelRange> {
        let anchor = self.anchor?;
        let current = addr?;
        let (start, end) = if anchor <= current { (anchor, current) } else { (current, anchor) };
        let start = source
            .item(GestureStage::Grapheme, start)
            .map(|item| item.start)
            .unwrap_or(start);
        let end = source
            .item(GestureStage::Grapheme, end)
            .map(|item| if item.start == end { end } else { item.end })
            .unwrap_or(end);
        (start < end).then_some(SelRange { start, end })
    }
    fn release(
        &mut self,
        button: Button,
        _local: PhysPoint,
        tick: u64,
        env: GestureEnv,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        if self.active_button != Some(button) {
            return Vec::new();
        }
        self.active_button = None;
        if self.dragging {
            self.dragging = false;
            self.stage = 0;
            self.anchor = None;
            self.press_point = None;
            self.current_link = false;
            self.last_press = None;
            self.pending_word = None;
            return vec![GestureEffect::DragEnded];
        }

        if self.stage == 1 {
            if button == Button::Primary && self.current_link {
                return vec![GestureEffect::OpenLink];
            }
            if additive(button, env) {
                self.pending_clear = Some(tick.saturating_add(env.chain_ticks));
            } else if !selection.is_empty() {
                selection.clear();
                return vec![GestureEffect::Repaint];
            }
        }
        Vec::new()
    }

    fn tick(&mut self, tick: u64, selection: &mut CardSelection) -> Vec<GestureEffect> {
        self.expire(tick, selection)
    }

    fn analysis(
        &mut self,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        let Some(pending) = self.pending_word.take() else { return Vec::new() };
        let Some(item) = source.item(GestureStage::Word, pending.addr) else {
            self.pending_word = Some(pending);
            return Vec::new();
        };
        self.apply(selection, item, pending.button, env);
        vec![GestureEffect::Repaint]
    }

    fn apply_stage(
        &mut self,
        addr: TextAddr,
        button: Button,
        env: GestureEnv,
        source: &ItemSource<'_>,
        selection: &mut CardSelection,
    ) -> Vec<GestureEffect> {
        let stage = match self.stage {
            2 => GestureStage::Word,
            3 => GestureStage::Sense,
            _ => GestureStage::Entry,
        };
        let Some(item) = source.item(stage, addr) else {
            if stage == GestureStage::Word {
                self.pending_word = Some(PendingWord { addr, button });
                return vec![GestureEffect::NeedWord {
                    entry: addr.entry,
                    path: addr.addr.path,
                    byte: addr.addr.byte,
                }];
            }
            return Vec::new();
        };
        self.apply(selection, item, button, env);
        vec![GestureEffect::Repaint]
    }


    fn apply(
        &self,
        selection: &mut CardSelection,
        item: SelRange,
        button: Button,
        env: GestureEnv,
    ) {
        *selection = self.snapshot.clone();
        if additive(button, env) {
            selection.toggle(item);
        } else {
            selection.replace(item);
        }
    }

    fn expire(&mut self, tick: u64, selection: &mut CardSelection) -> Vec<GestureEffect> {
        if self.pending_clear.is_some_and(|deadline| tick >= deadline) {
            self.pending_clear = None;
            if !selection.is_empty() {
                selection.clear();
                return vec![GestureEffect::Repaint];
            }
        }
        Vec::new()
    }
}

fn additive(button: Button, env: GestureEnv) -> bool {
    matches!(button, Button::Primary) == env.primary_additive
}

fn within(previous: PhysPoint, current: PhysPoint, threshold: i32) -> bool {
    let threshold = i64::from(threshold.max(0));
    (i64::from(current.x) - i64::from(previous.x)).abs() <= threshold
        && (i64::from(current.y) - i64::from(previous.y)).abs() <= threshold
}

fn beyond(previous: PhysPoint, current: PhysPoint, threshold: i32) -> bool {
    !within(previous, current, threshold)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const ENV: GestureEnv = GestureEnv {
        chain_ticks: 5,
        threshold_px: 4,
        primary_additive: true,
    };

    fn addr(byte: u32) -> TextAddr {
        TextAddr { entry: 0, addr: DocAddr { path: NodePath::ROOT.child(0).unwrap(), byte } }
    }

    fn range(start: u32, end: u32) -> SelRange {
        SelRange { start: addr(start), end: addr(end) }
    }

    fn input_press(tick: u64, addr: Option<TextAddr>, button: Button, link: bool) -> GestureInput {
        GestureInput::Press(PressInput {
            addr,
            link,
            button,
            local: PhysPoint { x: 10, y: 10 },
            tick,
        })
    }

    fn input_release(tick: u64, button: Button) -> GestureInput {
        GestureInput::Release {
            button,
            local: PhysPoint { x: 10, y: 10 },
            tick,
        }
    }

    #[test]
    fn click_chain_resolves_each_stage_from_one_snapshot() {
        let resolver = |stage: GestureStage, _addr: TextAddr| match stage {
            GestureStage::Grapheme => Some(range(0, 1)),
            GestureStage::Word => Some(range(0, 2)),
            GestureStage::Sense => Some(range(0, 3)),
            GestureStage::Entry => Some(range(0, 4)),
        };
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();

        let stages = [
            (1, None),
            (2, Some(range(0, 2))),
            (3, Some(range(0, 3))),
            (4, Some(range(0, 4))),
            (5, Some(range(0, 4))),
        ];
        for (tick, expected) in stages {
            let effects = gesture.handle(
                input_press(tick, Some(addr(0)), Button::Primary, false),
                ENV,
                &source,
                &mut selection,
            );
            if let Some(expected) = expected {
                assert_eq!(selection.items(), &[expected]);
                assert_eq!(effects, vec![GestureEffect::Repaint]);
            } else {
                assert!(effects.is_empty());
            }
            gesture.handle(input_release(tick, Button::Primary), ENV, &source, &mut selection);
        }
    }

    #[test]
    fn a_new_chain_snapshots_selection_and_a_late_press_does_not_chain() {
        let resolver = |_stage: GestureStage, _addr: TextAddr| Some(range(0, 2));
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        selection.replace(range(3, 4));

        gesture.handle(
            input_press(0, Some(addr(0)), Button::Primary, true),
            ENV,
            &source,
            &mut selection,
        );
        gesture.handle(input_release(0, Button::Primary), ENV, &source, &mut selection);
        let effects = gesture.handle(
            input_press(ENV.chain_ticks + 1, Some(addr(0)), Button::Primary, false),
            ENV,
            &source,
            &mut selection,
        );
        assert!(effects.is_empty());
        assert_eq!(selection.items(), &[range(3, 4)]);
    }

    #[test]
    fn drag_snaps_both_endpoints_and_ends_once() {
        let resolver = |stage: GestureStage, value: TextAddr| match stage {
            GestureStage::Grapheme => {
                let start = value.addr.byte / 2 * 2;
                Some(range(start, start + 2))
            }
            _ => None,
        };
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        gesture.handle(
            GestureInput::Press(PressInput {
                addr: Some(addr(1)),
                link: false,
                button: Button::Primary,
                local: PhysPoint { x: 0, y: 0 },
                tick: 0,
            }),
            ENV,
            &source,
            &mut selection,
        );
        assert_eq!(
            gesture.handle(
                GestureInput::Move {
                    addr: Some(addr(5)),
                    local: PhysPoint { x: 5, y: 0 },
                    tick: 1,
                },
                ENV,
                &source,
                &mut selection,
            ),
            vec![GestureEffect::DragStarted, GestureEffect::Repaint]
        );
        assert_eq!(selection.items(), &[range(0, 6)]);
        assert_eq!(
            gesture.handle(input_release(1, Button::Primary), ENV, &source, &mut selection),
            vec![GestureEffect::DragEnded]
        );
    }

    #[test]
    fn drag_snapping_respects_end_boundaries() {
        let boundary = [(0, 3), (3, 5)];
        let inside = [(0, 2), (2, 4)];
        let leftward = [(0, 2), (2, 4), (4, 6), (6, 8)];
        let cases = [
            (&boundary[..], 0, 3, range(0, 3)),
            (&inside[..], 0, 3, range(0, 4)),
            (&leftward[..], 6, 2, range(2, 6)),
        ];
        for (clusters, anchor, current, expected) in cases {
            let resolver = |stage: GestureStage, value: TextAddr| {
                if stage != GestureStage::Grapheme {
                    return None;
                }
                clusters
                    .iter()
                    .copied()
                    .find(|(start, end)| *start <= value.addr.byte && value.addr.byte < *end)
                    .map(|(start, end)| range(start, end))
            };
            let source = ItemSource::from_fn(&resolver);
            let gesture = Gesture { anchor: Some(addr(anchor)), ..Gesture::default() };
            assert_eq!(gesture.drag_item(Some(addr(current)), &source), Some(expected));
        }
    }

    #[test]
    fn plain_click_clear_can_expire_or_be_cancelled() {
        let resolver = |_stage: GestureStage, _addr: TextAddr| Some(range(0, 2));
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        selection.replace(range(3, 4));
        gesture.handle(
            input_press(0, Some(addr(0)), Button::Primary, false),
            ENV,
            &source,
            &mut selection,
        );
        gesture.handle(input_release(0, Button::Primary), ENV, &source, &mut selection);
        assert_eq!(
            gesture.handle(GestureInput::Tick { tick: ENV.chain_ticks }, ENV, &source, &mut selection),
            vec![GestureEffect::Repaint]
        );
        assert!(selection.is_empty());

        selection.replace(range(3, 4));
        gesture.handle(
            input_press(10, Some(addr(0)), Button::Primary, false),
            ENV,
            &source,
            &mut selection,
        );
        gesture.handle(input_release(10, Button::Primary), ENV, &source, &mut selection);
        let effects = gesture.handle(
            input_press(11, Some(addr(0)), Button::Primary, false),
            ENV,
            &source,
            &mut selection,
        );
        assert_eq!(effects, vec![GestureEffect::Repaint]);
        assert_eq!(selection.items(), &[range(0, 2), range(3, 4)]);
        assert!(gesture
            .handle(GestureInput::Tick { tick: 15 }, ENV, &source, &mut selection)
            .is_empty());
    }

    #[test]
    fn a_nonchained_press_clears_pending_plain_click_before_drag() {
        let resolver = |stage: GestureStage, value: TextAddr| {
            if stage == GestureStage::Grapheme {
                Some(range(value.addr.byte, value.addr.byte.saturating_add(1)))
            } else {
                None
            }
        };
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        selection.replace(range(8, 9));

        gesture.handle(
            input_press(0, Some(addr(0)), Button::Primary, false),
            ENV,
            &source,
            &mut selection,
        );
        gesture.handle(input_release(0, Button::Primary), ENV, &source, &mut selection);
        assert_eq!(
            gesture.handle(
                GestureInput::Press(PressInput {
                    addr: Some(addr(2)),
                    link: false,
                    button: Button::Primary,
                    local: PhysPoint { x: 20, y: 10 },
                    tick: 1,
                }),
                ENV,
                &source,
                &mut selection,
            ),
            vec![GestureEffect::Repaint]
        );
        assert_eq!(
            gesture.handle(
                GestureInput::Move {
                    addr: Some(addr(4)),
                    local: PhysPoint { x: 30, y: 10 },
                    tick: 2,
                },
                ENV,
                &source,
                &mut selection,
            ),
            vec![GestureEffect::DragStarted, GestureEffect::Repaint]
        );
        assert_eq!(selection.items(), &[range(2, 4)]);
        assert!(gesture
            .handle(GestureInput::Tick { tick: ENV.chain_ticks }, ENV, &source, &mut selection)
            .is_empty());
        assert_eq!(selection.items(), &[range(2, 4)]);
    }

    #[test]
    fn replacing_plain_click_clears_immediately() {
        let resolver = |_stage: GestureStage, _addr: TextAddr| Some(range(0, 2));
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        selection.replace(range(3, 4));
        gesture.handle(
            input_press(0, Some(addr(0)), Button::Secondary, false),
            ENV,
            &source,
            &mut selection,
        );
        assert_eq!(
            gesture.handle(input_release(0, Button::Secondary), ENV, &source, &mut selection),
            vec![GestureEffect::Repaint]
        );
        assert!(selection.is_empty());
    }

    #[test]
    fn a_word_waits_for_analysis() {
        let ready = Cell::new(false);
        let resolver = |stage: GestureStage, _addr: TextAddr| {
            (stage != GestureStage::Word || ready.get()).then_some(range(0, 2))
        };
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        assert_eq!(
            gesture.handle(
                input_press(0, Some(addr(0)), Button::Primary, false),
                ENV,
                &source,
                &mut selection,
            ),
            Vec::<GestureEffect>::new()
        );
        gesture.handle(input_release(0, Button::Primary), ENV, &source, &mut selection);
        assert_eq!(
            gesture.handle(
                input_press(1, Some(addr(0)), Button::Primary, false),
                ENV,
                &source,
                &mut selection,
            ),
            vec![GestureEffect::NeedWord { entry: 0, path: addr(0).addr.path, byte: 0 }]
        );
        ready.set(true);
        assert_eq!(
            gesture.handle(GestureInput::Analysis, ENV, &source, &mut selection),
            vec![GestureEffect::Repaint]
        );
        assert_eq!(selection.items(), &[range(0, 2)]);
    }

    #[test]
    fn links_open_only_when_a_plain_primary_click_survives() {
        let resolver = |_stage: GestureStage, _addr: TextAddr| Some(range(0, 2));
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();
        gesture.handle(
            input_press(0, Some(addr(0)), Button::Primary, true),
            ENV,
            &source,
            &mut selection,
        );
        assert_eq!(
            gesture.handle(input_release(0, Button::Primary), ENV, &source, &mut selection),
            vec![GestureEffect::OpenLink]
        );

        gesture.handle(
            GestureInput::Press(PressInput {
                addr: Some(addr(0)),
                link: true,
                button: Button::Primary,
                local: PhysPoint { x: 0, y: 0 },
                tick: 10,
            }),
            ENV,
            &source,
            &mut selection,
        );
        gesture.handle(
            GestureInput::Move {
                addr: Some(addr(2)),
                local: PhysPoint { x: 10, y: 0 },
                tick: 11,
            },
            ENV,
            &source,
            &mut selection,
        );
        assert_eq!(
            gesture.handle(input_release(11, Button::Primary), ENV, &source, &mut selection),
            vec![GestureEffect::DragEnded]
        );
    }

    #[test]
    fn a_link_without_text_keeps_no_anchor_and_cannot_drag() {
        let resolver = |_stage: GestureStage, _addr: TextAddr| Some(range(0, 2));
        let source = ItemSource::from_fn(&resolver);
        let mut gesture = Gesture::default();
        let mut selection = CardSelection::default();

        gesture.handle(
            GestureInput::Press(PressInput {
                addr: None,
                link: true,
                button: Button::Primary,
                local: PhysPoint { x: 0, y: 0 },
                tick: 0,
            }),
            ENV,
            &source,
            &mut selection,
        );
        assert!(gesture.anchor.is_none());
        assert!(gesture
            .handle(
                GestureInput::Move {
                    addr: Some(addr(2)),
                    local: PhysPoint { x: 10, y: 0 },
                    tick: 1,
                },
                ENV,
                &source,
                &mut selection,
            )
            .is_empty());
        assert!(!gesture.dragging());
        assert_eq!(
            gesture.handle(input_release(1, Button::Primary), ENV, &source, &mut selection),
            vec![GestureEffect::OpenLink]
        );
    }
}
