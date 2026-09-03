//! Both platform bins use one shared Card selection model.
//!
//! Selection state lives in core, so Windows and Linux use the same containment,
//! clipping, and coverage rules. FakeMeasure tests cover this model through the
//! same measured text seam that the Controller and layout use. This design
//! follows the issue's note for future input changes. Later input changes need
//! one selection rule.
//!
//! A `DocAddr` is a role-visible leaf path and byte offset. A [`TextAddr`]
//! identifies one position in a Card. It stores that address with its flattened
//! Entry ordinal.

pub use crate::dict::gloss::{DocAddr, DocRange, Separator};
pub mod gesture;
pub use gesture::{
    Button, Button as GestureButton, Gesture, GestureEffect, GestureEnv, GestureInput, GestureStage,
    ItemSource, Stage,
};

use crate::present::{Card, GlossEntry};

/// One position in the current Card.
///
/// `entry` is the `GlossEntry` ordinal in flattened display order. `addr` is a
/// document-order address within that Entry's `GlossDoc`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TextAddr {
    pub entry: u32,
    pub addr: DocAddr,
}

/// A half-open selected range.
///
/// A CardSelection stores only ranges where `start < end`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelRange {
    pub start: TextAddr,
    pub end: TextAddr,
}

/// The selected ranges for one Card, kept in start order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CardSelection {
    items: Vec<SelRange>,
}

/// How much of an Entry or Card a selection covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coverage {
    None,
    Partial,
    All,
}

/// Selection state for every Card in the `Presentation`.
///
/// The vector index is the index in `Presentation::all_cards`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Selections {
    pub cards: Vec<CardSelection>,
}

impl CardSelection {
    /// Reports whether this Card has no selected range.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the selected ranges in start order.
    pub fn items(&self) -> &[SelRange] {
        &self.items
    }

    /// Removes every selected range from this Card.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Applies an additive selection gesture.
    ///
    /// An identical range removes the existing range. A range that contains
    /// selected ranges absorbs them. A range inside one or more selected ranges
    /// replaces those ranges. The method adds a range that intersects the
    /// existing ranges but contains none of them.
    pub fn toggle(&mut self, item: SelRange) {
        if !valid_range(item) {
            return;
        }

        if let Some(index) = self.items.iter().position(|&selected| selected == item) {
            self.items.remove(index);
            return;
        }

        let item_contains_selected = self.items.iter().any(|&selected| contains(item, selected));
        let selected_contains_item = self.items.iter().any(|&selected| contains(selected, item));

        if item_contains_selected || selected_contains_item {
            self.items.retain(|&selected| {
                !contains(item, selected) && !contains(selected, item)
            });
        }
        self.items.push(item);
        sort_items(&mut self.items);
    }

    /// Replaces the Card selection with one range.
    pub fn replace(&mut self, item: SelRange) {
        self.clear();
        if valid_range(item) {
            self.items.push(item);
        }
    }

    /// Returns the union clipped to one Entry.
    pub fn entry_ranges(&self, entry: u32) -> Vec<DocRange> {
        let first = TextAddr {
            entry,
            addr: DocAddr::START,
        };
        let last = TextAddr {
            entry,
            addr: DocAddr::END,
        };
        let mut ranges = Vec::new();

        for item in self.union() {
            if item.end <= first || item.start >= last {
                continue;
            }
            let start = if item.start.entry < entry {
                DocAddr::START
            } else {
                item.start.addr
            };
            let end = if item.end.entry > entry {
                DocAddr::END
            } else {
                item.end.addr
            };
            if start < end {
                ranges.push(DocRange { start, end });
            }
        }
        ranges
    }

    /// Reports coverage of `extent` in one Entry.
    pub fn coverage(&self, entry: u32, extent: DocRange) -> Coverage {
        if extent.start >= extent.end {
            return Coverage::None;
        }

        let mut covered_to = extent.start;
        let mut intersects = false;
        for range in self.entry_ranges(entry) {
            if range.end <= extent.start || range.start >= extent.end {
                continue;
            }
            intersects = true;
            if range.start <= covered_to {
                if range.end > covered_to {
                    covered_to = range.end;
                }
                if covered_to >= extent.end {
                    return Coverage::All;
                }
            }
        }

        if intersects {
            Coverage::Partial
        } else {
            Coverage::None
        }
    }
    /// Removes ranges that touch Entry `entry`. When `on` is true, selects its
    /// whole extent.
    pub fn set_entry(&mut self, entry: u32, extent: DocRange, on: bool) {
        let first = TextAddr {
            entry,
            addr: DocAddr::START,
        };
        let last = TextAddr {
            entry,
            addr: DocAddr::END,
        };
        self.items.retain(|&item| {
            valid_range(item) && (item.end <= first || item.start >= last)
        });
        if on && extent.start < extent.end {
            self.items.push(SelRange {
                start: TextAddr {
                    entry,
                    addr: extent.start,
                },
                end: TextAddr {
                    entry,
                    addr: extent.end,
                },
            });
            sort_items(&mut self.items);
        }
    }

    /// Returns the sorted, merged ranges with no overlap.
    pub fn union(&self) -> Vec<SelRange> {
        let mut ranges: Vec<SelRange> = self
            .items
            .iter()
            .copied()
            .filter(|&item| valid_range(item))
            .collect();
        sort_items(&mut ranges);

        let mut merged: Vec<SelRange> = Vec::with_capacity(ranges.len());
        for item in ranges {
            if let Some(last) = merged.last_mut() {
                if item.start <= last.end {
                    if item.end > last.end {
                        last.end = item.end;
                    }
                    continue;
                }
            }
            merged.push(item);
        }
        merged
    }
}

impl Selections {
    /// Returns the selection for Card `i` when it exists.
    pub fn card(&self, i: usize) -> Option<&CardSelection> {
        self.cards.get(i)
    }

    /// Returns the selection for Card `i`. Extends the vector with empty Card
    /// selections when needed.
    pub fn card_mut(&mut self, i: usize) -> &mut CardSelection {
        if self.cards.len() <= i {
            self.cards.resize_with(i + 1, CardSelection::default);
        }
        &mut self.cards[i]
    }

    /// Swaps two Card selections when both indexes exist.
    pub fn swap(&mut self, a: usize, b: usize) {
        if a < self.cards.len() && b < self.cards.len() {
            self.cards.swap(a, b);
        }
    }

    /// Reports aggregate coverage for a Card's Entry extents.
    pub fn coverage_of_card(&self, i: usize, extents: &[DocRange]) -> Coverage {
        let Some(card) = self.card(i) else {
            return Coverage::None;
        };
        if card.is_empty() || extents.is_empty() {
            return Coverage::None;
        }
        if extents
            .iter()
            .enumerate()
            .all(|(entry, &extent)| card.coverage(entry as u32, extent) == Coverage::All)
        {
            Coverage::All
        } else {
            Coverage::Partial
        }
    }
}

/// Iterates through each Entry in the order of its block and its position in
/// that block.
pub fn entries(card: &Card) -> impl Iterator<Item = (u32, &GlossEntry)> {
    card.blocks
        .iter()
        .flat_map(|block| block.entries.iter())
        .enumerate()
        .map(|(ordinal, entry)| (ordinal as u32, entry))
}

fn valid_range(item: SelRange) -> bool {
    item.start < item.end
}

fn contains(outer: SelRange, inner: SelRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn sort_items(items: &mut [SelRange]) {
    items.sort_by_key(|item| item.start);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::dict::gloss::{GlossDoc, NodePath};
    use crate::present::{GlossBlock, GlossEntry};

    fn addr(entry: u32, byte: u32) -> TextAddr {
        TextAddr {
            entry,
            addr: DocAddr {
                path: NodePath::ROOT,
                byte,
            },
        }
    }

    fn range(start_entry: u32, start: u32, end_entry: u32, end: u32) -> SelRange {
        SelRange {
            start: addr(start_entry, start),
            end: addr(end_entry, end),
        }
    }

    fn extent(start: u32, end: u32) -> DocRange {
        DocRange {
            start: DocAddr {
                path: NodePath::ROOT,
                byte: start,
            },
            end: DocAddr {
                path: NodePath::ROOT,
                byte: end,
            },
        }
    }

    fn test_card() -> Card {
        let doc = Arc::new(GlossDoc::default());
        let entry = |entry_id| GlossEntry {
            entry_id,
            glosses: Vec::new(),
            tags: Vec::new(),
            doc: doc.clone(),
            media: Vec::new(),
        };
        Card {
            written: None,
            reading: None,
            pos: Vec::new(),
            freq: None,
            blocks: vec![
                GlossBlock {
                    dict_name: "first".to_string(),
                    dict_id: 1,
                    entries: vec![entry(11), entry(12)],
                },
                GlossBlock {
                    dict_name: "second".to_string(),
                    dict_id: 2,
                    entries: vec![entry(21)],
                },
            ],
            match_len: 0,
            pitch: Vec::new(),
        }
    }

    #[test]
    fn entries_flattens_blocks_in_display_order() {
        let card = test_card();
        let ids: Vec<_> = entries(&card).map(|(ordinal, entry)| (ordinal, entry.entry_id)).collect();
        assert_eq!(vec![(0, 11), (1, 12), (2, 21)], ids);
    }

    #[test]
    fn toggle_table_applies_containment_rules() {
        let cases = vec![
            (range(0, 2, 0, 4), range(0, 2, 0, 4), Vec::new()),
            (range(0, 1, 0, 9), range(0, 2, 0, 8), vec![range(0, 2, 0, 8)]),
            (range(0, 2, 0, 4), range(0, 1, 0, 5), vec![range(0, 1, 0, 5)]),
            (
                range(0, 1, 0, 4),
                range(0, 3, 0, 7),
                vec![range(0, 1, 0, 4), range(0, 3, 0, 7)],
            ),
        ];

        for (old, new, expected) in cases {
            let mut selection = CardSelection::default();
            selection.replace(old);
            selection.toggle(new);
            assert_eq!(expected, selection.items());
        }
    }

    #[test]
    fn replace_clears_first() {
        let mut selection = CardSelection::default();
        selection.replace(range(0, 1, 0, 2));
        selection.replace(range(1, 1, 1, 3));
        assert_eq!(&[range(1, 1, 1, 3)], selection.items());
    }

    #[test]
    fn entry_ranges_clip_a_range_across_three_entries() {
        let mut selection = CardSelection::default();
        selection.replace(range(0, 3, 2, 4));

        assert_eq!(
            vec![DocRange {
                start: DocAddr {
                    path: NodePath::ROOT,
                    byte: 3,
                },
                end: DocAddr::END,
            }],
            selection.entry_ranges(0)
        );
        assert_eq!(
            vec![DocRange {
                start: DocAddr::START,
                end: DocAddr::END,
            }],
            selection.entry_ranges(1)
        );
        assert_eq!(
            vec![DocRange {
                start: DocAddr::START,
                end: DocAddr {
                    path: NodePath::ROOT,
                    byte: 4,
                },
            }],
            selection.entry_ranges(2)
        );
    }

    #[test]
    fn coverage_reports_none_partial_and_all() {
        let full = extent(0, 10);
        let cases = [
            (Coverage::None, Vec::new()),
            (Coverage::Partial, vec![range(0, 2, 0, 5)]),
            (Coverage::All, vec![range(0, 0, 0, 10)]),
        ];
        for (expected, items) in cases {
            let mut selection = CardSelection::default();
            for item in items {
                selection.toggle(item);
            }
            assert_eq!(expected, selection.coverage(0, full));
        }
    }

    #[test]
    fn set_entry_turns_selection_on_and_off() {
        let full = extent(0, 10);
        let mut selection = CardSelection::default();
        selection.set_entry(0, full, true);
        assert_eq!(Coverage::All, selection.coverage(0, full));
        selection.set_entry(0, full, false);
        assert!(selection.is_empty());
    }

    #[test]
    fn card_coverage_is_none_partial_or_all() {
        let extents = [extent(0, 10), extent(0, 10)];
        let mut selections = Selections::default();
        assert_eq!(Coverage::None, selections.coverage_of_card(0, &extents));

        selections.card_mut(0).replace(range(0, 0, 1, 10));
        assert_eq!(Coverage::All, selections.coverage_of_card(0, &extents));

        selections.card_mut(0).replace(range(0, 0, 0, 5));
        assert_eq!(Coverage::Partial, selections.coverage_of_card(0, &extents));
    }

    #[test]
    fn swap_mirrors_swap_top_indexes() {
        let mut selections = Selections::default();
        selections.card_mut(0).replace(range(0, 0, 0, 1));
        selections.card_mut(1).replace(range(1, 0, 1, 1));
        selections.card_mut(2).replace(range(2, 0, 2, 1));
        selections.swap(0, 1);
        assert_eq!(Some(&[range(1, 0, 1, 1)][..]), selections.card(0).map(CardSelection::items));
        assert_eq!(Some(&[range(0, 0, 0, 1)][..]), selections.card(1).map(CardSelection::items));
    }

    #[test]
    fn union_merges_overlapping_and_touching_ranges() {
        let mut selection = CardSelection::default();
        selection.replace(range(0, 5, 0, 8));
        selection.toggle(range(0, 1, 0, 5));
        selection.toggle(range(0, 8, 0, 10));
        assert_eq!(vec![range(0, 1, 0, 10)], selection.union());
    }
}
