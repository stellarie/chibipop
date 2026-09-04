//! This module builds the popup content for one Entry and the chrome around it.
//!
//! **One reason to change:** Change this module when the panel content changes.
//! The panel can add or remove a row, change a heading, or reserve space for a control.
//!
//! [`build_elements`] is the only function that reads render settings.
//! This keeps [`RenderSettings`] as a decision table instead of a cascade.
//! [`build_elements`] resolves four facts into the two values used by the gloss walk.
//! Code after that point does not read a setting.
//!
//! Every element here belongs to the panel, not to a Dictionary.
//! Each chrome element has one style and one span.
//! Each element uses the seam as before the inline pass.
use crate::dict::gloss::{extent, DocAddr, DocRange, RoleFilter};
use crate::dict::pitch::marked_morae;
use crate::present::{AnkiPopupState, Card, PitchRow, Presentation};
use crate::select::{Coverage, Selections};
use crate::ui::theme::Theme;
use super::flow::Flow;
use super::gloss::{paragraphs, Assets, Render};
use super::measure::{
    measure_text, GlyphBox, MeasureError, MeasureRun, Measured, Metrics, StyledSpan, TextMeasure,
};
use super::pass::{Boxed, Piece};
use super::scene::{
    Align, ElemBox, ElemKind, ElemSpan, Rgb, SceneElem, SceneRect, SidePanel, SideRow,
};
use super::style::{BorderStyle, BoxStyle, Edges, Inline};
use super::table::Grid;

/// Gap within a block.
pub(super) const LINE_GAP: f32 = 4.0;

/// Gap before a new block.
pub(super) const SECTION_GAP: f32 = 10.0;

/// Gap beside the corner element.
pub(super) const CORNER_GAP: f32 = 8.0;

/// Gap around the rule.
pub(super) const SEPARATOR_MARGIN: f32 = 10.0;

/// Width of the vertical rule in the side column.
///
/// `Theme::separator_height` sets the height of the horizontal rule between blocks.
/// It cannot set the width of a vertical rule.
pub(super) const SEPARATOR_THICKNESS: f32 = 1.0;

/// Thickness of the overline and downstep tick in pixels.
///
/// This value gives the full ink thickness for marked kana.
/// Yomitan draws `border-top` and `border-right` at one pixel.
/// This value matches that width.
/// A thicker mark looks like a highlight instead of a pitch mark.
pub(super) const PITCH_MARK: f32 = 1.0;

/// Space between a marked reading and the Dictionaries that report its accent.
///
/// This constant is one U+2003 EM SPACE.
/// It reserves space but draws no glyph.
/// The collapsed row uses an em dash between its headword and summary because they are separate.
/// A reading and its Dictionary sources form one statement, so one space separates them.
pub(super) const PITCH_SOURCE_GAP: &str = "\u{2003}";

/// Fixed "See also" column width.
pub(super) const SIDE_PANEL_W: f32 = 110.0;

/// Gap before the side panel.
pub(super) const SIDE_GAP: f32 = 12.0;

/// Popup content in the form that the scene builder uses.
///
/// Render settings form a decision table, not a cascade of style rules.
/// Yomitan lets a user set a fixed group of root attributes.
/// Those attributes drive fixed CSS rules.
/// This struct mirrors that design.
/// `PopupConfig::render_settings` resolves six `popup` settings into four facts.
/// The gloss walk uses those facts.
/// The enum-to-flag table stays in that function.
/// The gloss walk does not see a `PopupConfig` value.
///
/// [`build_elements`] reads this struct in one place.
/// This keeps [`RenderSettings`] as a decision table instead of a cascade.
/// Code after that point does not read a setting again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSettings {
    /// Does a glossary list place each item on a separate line?
    ///
    /// `true` gives each item its own line with a marker and an indent.
    /// `false` joins the items into one paragraph with [`ITEM_SEPARATOR`] between them.
    /// This was the plain form that chibipop used before the panel could stack a list.
    /// This is the same parameter that the list pass already had ([`Paragraphs::stack_items`]).
    /// Compact mode changes only this value.
    /// It adds no other behavior.
    ///
    /// [`ITEM_SEPARATOR`]: super::gloss::ITEM_SEPARATOR
    /// [`Paragraphs::stack_items`]: super::gloss::Paragraphs::stack_items
    pub stack_items: bool,
    /// Do a Dictionary's style declarations apply?
    ///
    /// Off draws every Entry with the theme's font and colors.
    /// The same flag controls both style sources.
    /// The Dictionary's inline `style` attribute and its `styles.css` file resolve into one record.
    /// [`Paragraphs::declarations`] is the only gate for that record.
    ///
    /// [`Paragraphs::declarations`]: super::gloss::Paragraphs::declarations
    pub styling: bool,
    /// Do image assets reach the panel?
    ///
    /// Off keeps the `alt` text in place.
    /// An image node usually represents a character, not an illustration.
    /// Removing the whole node would leave a hole in a word.
    pub images: bool,
    /// Which editorial roles reach the panel?
    ///
    /// This field reuses the card renderer's `RoleFilter` type.
    /// It stores a separate value of that type.
    /// The popup filter and the card filter are two values of one enum.
    /// This lets a user hide examples on screen but retain them on a mined card.
    /// The card filter stays at [`RoleFilter::CARD`]. No setting changes it.
    pub roles: RoleFilter,
}

impl Default for RenderSettings {
    /// Values that a fresh install draws:
    ///
    /// - A stacked list.
    /// - The Dictionary's style.
    /// - The Dictionary's images.
    /// - The Dictionary's editorial matter except part-of-speech labels.
    ///   The card already prints those labels above the glosses.
    ///
    /// `PopupConfig` defaults resolve to these values.
    /// A test checks that both sets match.
    /// A geometry fixture and a fresh install therefore draw the same panel.
    fn default() -> RenderSettings {
        RenderSettings {
            stack_items: true,
            styling: true,
            images: true,
            roles: RoleFilter::CARD,
        }
    }
}

/// Shared shape for a text element.
pub(super) fn text_elem(
    kind: ElemKind,
    line: &Line,
    met: &Metrics,
    origin: f32,
    y: f32,
    wrap_w: f32,
) -> SceneElem {
    SceneElem {
        kind,
        text: line.text.clone(),
        color: line.color,
        font_size: line.size,
        weight: line.weight,
        italic: line.italic,
        top_gap: line.top_gap,
        wrap_w,
        align: Align::Leading,
        pen: (origin, origin + y),
        rect: SceneRect { x: origin, y: origin + y, w: met.w, h: met.h },
        lines: met.lines,
        advance: met.h,
        spans: one_span(line),
        ruby: Vec::new(),
        marker: Vec::new(),
        block_box: None,
        inline_boxes: Vec::new(),
        // This element belongs to the panel chrome, not gloss content.
        // A headword, reading, Dictionary label, or collapsed row comes from a
        // `Presentation`, not from a parsed tree.
        // None of these elements can address a node.
        // Chrome elements do not use an asset.
        origin: None,
        image: None,
        sources: Vec::new(),
    }
}

/// The one span that a `Line` becomes in the scene.
///
/// Every element that the panel chrome builds has one style and one span.
/// The seam receives the same request that it received before the inline pass.
pub(super) fn one_span(line: &Line) -> Vec<ElemSpan> {
    vec![ElemSpan {
        at: 0,
        len: line.text.len() as u32,
        color: line.color,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        shift: 0.0,
    }]
}

/// The one span that a `Line` becomes for the seam.
///
/// Every panel chrome element has one style.
/// A gloss paragraph is the only element that gives the seam more than one span.
/// It does so through [`Flow::styled_spans`].
pub(super) fn span<'a>(font: &'a str, line: &'a Line) -> StyledSpan<'a> {
    StyledSpan {
        text: &line.text,
        font,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        color: line.color,
    }
}

/// Measures one styled line's box.
pub(super) fn measure_line(
    m: &mut dyn TextMeasure,
    font: &str,
    line: &Line,
    max_w: f32,
    scratch: &mut Measured,
) -> Result<Metrics, MeasureError> {
    measure_text(m, span(font, line), max_w, scratch)
}

/// One "See also" row's span.
///
/// The collapsed row supplies the role for the whole column.
/// Rows differ only in text and color.
/// Color does not affect geometry.
pub(super) fn side_span<'a>(theme: &'a Theme, text: &'a str, color: Rgb) -> StyledSpan<'a> {
    StyledSpan {
        text,
        font: theme.font_name.as_str(),
        size: theme.collapsed_size,
        weight: theme.collapsed_weight,
        italic: theme.collapsed_italic,
        color,
    }
}

/// Measures one pitch row and marks its accent.
///
/// This element has two spans.
/// Every other panel chrome element has one span.
/// The reading uses the card reading style.
/// Dictionaries that report the accent use dimmed text beside the reading.
/// The reading is the first span.
/// Therefore, a UTF-16 offset in the run equals the offset in the reading.
///
/// The marks come from [`TextMeasure::caret_boxes`].
/// [`TextMeasure::caret_boxes`] also produces the headword's per-character hit targets.
/// Only the measurer knows each mora position.
/// The code probes every character in the reading, not only each mora's first character.
/// A mora has one or two characters, so `きょ` uses both boxes.
/// The first character's box would leave half the mora unmarked.
///
/// The code draws one box for each marked mora, not one box for each run.
/// Adjacent moras touch, so boxes in a run appear as one overline.
/// A reading that wraps across two lines gets each mora's mark on its own line.
/// The code does not stretch a box across the line break.
///
/// The `pen` parameter gives the row's absolute position.
/// The gloss walk and the table walk pass this position.
/// The function also needs the measurer, the text, and two scratch buffers that the seam fills.
/// No smaller parameter group exists.
/// A smaller group would repeat this function's own parameters.
pub(super) fn pitch_elem(
    m: &mut dyn TextMeasure,
    font: &str,
    pitch: &Pitch,
    pen: (f32, f32),
    wrap_w: f32,
    scratch: &mut Measured,
    probes: &mut Vec<GlyphBox>,
) -> Result<SceneElem, MeasureError> {
    let spans = [span(font, &pitch.reading), span(font, &pitch.sources)];
    let run = MeasureRun { spans: &spans, max_w: wrap_w };
    m.measure(run, scratch)?;
    let met = scratch.metrics;

    let mut at: Vec<u32> = Vec::with_capacity(pitch.reading.text.len());
    let mut unit = 0u32;
    for c in pitch.reading.text.chars() {
        at.push(unit);
        unit += c.len_utf16() as u32;
    }
    probes.clear();
    m.caret_boxes(run, &at, probes)?;

    let mut inline_boxes = Vec::new();
    for mora in pitch.morae.iter().filter(|mora| mora.high) {
        let mut covered = at
            .iter()
            .zip(probes.iter())
            .filter(|(offset, _)| (mora.at..mora.at + mora.units).contains(offset))
            .map(|(_, probed)| *probed);
        let Some(first) = covered.next() else { continue };
        let last = covered.next_back().unwrap_or(first);
        inline_boxes.push(ElemBox {
            rect: SceneRect {
                x: pen.0 + first.x,
                y: pen.1 + first.y,
                w: (last.x + last.w - first.x).max(0.0),
                h: first.h,
            },
            style: mark_style(pitch.reading.color, mora.fall),
        });
    }

    let split = pitch.reading.text.len() as u32;
    Ok(SceneElem {
        kind: ElemKind::Pitch,
        text: format!("{}{}", pitch.reading.text, pitch.sources.text),
        color: pitch.reading.color,
        font_size: pitch.reading.size,
        weight: pitch.reading.weight,
        italic: pitch.reading.italic,
        top_gap: pitch.reading.top_gap,
        wrap_w,
        align: Align::Leading,
        pen,
        rect: SceneRect { x: pen.0, y: pen.1, w: met.w, h: met.h },
        lines: met.lines,
        advance: met.h,
        spans: vec![
            elem_span(0, split, &pitch.reading),
            elem_span(split, pitch.sources.text.len() as u32, &pitch.sources),
        ],
        ruby: Vec::new(),
        marker: Vec::new(),
        block_box: None,
        inline_boxes,
        // This belongs to the panel chrome.
        // An accent belongs to a reading, not to a tree node, so it has no node address.
        origin: None,
        image: None,
        sources: Vec::new(),
    })
}

/// Builds one span from a `Line` for an element that holds several spans.
fn elem_span(at: u32, len: u32, line: &Line) -> ElemSpan {
    ElemSpan {
        at,
        len,
        color: line.color,
        size: line.size,
        weight: line.weight,
        italic: line.italic,
        shift: 0.0,
    }
}

/// Border edges that draw one marked mora.
///
/// A top edge sits over each high mora, so adjacent high moras form one overline.
/// A right edge marks the mora after the pitch falls.
/// This edge is the downstep tick.
/// The style sets no padding, margin, or background.
/// A box that only decorates does not take space from its line.
/// A pitch row therefore measures and stacks like the same text without an accent.
fn mark_style(color: Rgb, fall: bool) -> BoxStyle {
    let tick = if fall { PITCH_MARK } else { 0.0 };
    let tick_style = if fall { BorderStyle::Solid } else { BorderStyle::None };
    BoxStyle {
        border: Edges { top: PITCH_MARK, right: tick, ..Edges::default() },
        border_style: Edges {
            top: BorderStyle::Solid,
            right: tick_style,
            ..Edges::default()
        },
        border_color: color,
        ..BoxStyle::default()
    }
}

/// Geometry for the "See also" column.
pub(super) fn side_panel(
    entries: &[SideEntry],
    theme: &Theme,
    origin: f32,
    content_w: f32,
    m: &mut dyn TextMeasure,
    scratch: &mut Measured,
) -> Result<SidePanel, MeasureError> {
    let heading = side_span(theme, SIDE_HEADING, theme.dimmed_text);
    let head = measure_text(m, heading, SIDE_PANEL_W, scratch)?;

    let mut rows = Vec::with_capacity(entries.len() + 1);
    rows.push(SideRow {
        idx: None,
        text: SIDE_HEADING.to_string(),
        color: theme.dimmed_text,
        y: 0.0,
        h: head.h,
    });
    let mut y = head.h + LINE_GAP;

    for entry in entries {
        let met =
            measure_text(m, side_span(theme, &entry.text, entry.color), SIDE_PANEL_W, scratch)?;
        rows.push(SideRow {
            idx: Some(entry.idx),
            text: entry.text.clone(),
            color: entry.color,
            y,
            h: met.h,
        });
        y += LINE_GAP + met.h;
    }

    let rule_x = origin + content_w + SIDE_GAP;
    Ok(SidePanel {
        origin_y: origin,
        rule_x,
        rule_w: SEPARATOR_THICKNESS,
        col_x: rule_x + SEPARATOR_THICKNESS + SIDE_GAP,
        col_w: SIDE_PANEL_W,
        height: y,
        rows,
    })
}

/// The side column's heading.
pub(super) const SIDE_HEADING: &str = "See also";

/// One text line that the code can measure or draw.
///
/// The code creates a new `Line` each time.
/// It does not cache this structure.
#[derive(Clone)]
pub(super) struct Line {
    pub(super) text: String,
    pub(super) color: Rgb,
    pub(super) size: f32,
    /// Extra space above this line.
    pub(super) top_gap: f32,
    /// DirectWrite weight from 100 through 900.
    pub(super) weight: u16,
    pub(super) italic: bool,
}

/// One accent represented as marked kana.
///
/// The marks are notation, not decoration.
/// An overline sits over each high mora.
/// A tick marks the right edge of the mora after which the pitch falls.
/// Both marks are one border edge of an inline box.
/// Yomitan's stylesheet draws the marks with the same border edges.
/// Therefore, chibipop draws pitch in its text stack and needs no SVG.
pub(super) struct Pitch {
    /// The reading in the card's own reading style.
    /// The marks sit over this text.
    /// This is the element's first span.
    /// Dimmed names of the Dictionaries that report this accent, after [`PITCH_SOURCE_GAP`].
    /// This is the element's second span.
    /// The code does not read its `top_gap`.
    /// The row gets its gap from the reading's `top_gap`.
    pub(super) reading: Line,
    pub(super) sources: Line,
    /// The reading's moras with their mark state, in order.
    pub(super) morae: Vec<Marked>,
}

/// One mora of a marked reading in the coordinates that the seam addresses.
///
/// This struct copies the value that [`marked_morae`] computes.
/// It does not borrow that value.
/// A mora has one or two *characters*, but the measurer addresses a run by UTF-16 unit.
/// The code cannot derive either count from the other, so it stores both.
pub(super) struct Marked {
    /// UTF-16 offset of the mora's first unit, into the reading.
    pub(super) at: u32,
    /// UTF-16 units in it.
    pub(super) units: u32,
    pub(super) high: bool,
    /// Does the pitch fall after this mora?
    pub(super) fall: bool,
}

pub(super) enum Elem {
    Text(Line),
    /// Draws right-aligned without advancing the layout's y position.
    ///
    /// It removes width from the next element.
    Corner(Line),
    Separator { top_gap: f32 },
    /// A Check element that uses the next line's top gap.
    Check {
        entry: Option<u32>,
        coverage: Coverage,
        top_gap: f32,
    },
    /// A collapsed row that accepts clicks.
    Collapsed(usize, Line),
    /// One click target for each character.
    Headword {
        headword: String,
        prefix_u16: usize,
        line: Line,
    },
    /// One accent represented as marked kana.
    ///
    /// This element has two spans and one box for each marked mora.
    /// It is the only chrome element with more than one style ([`Pitch`]).
    Pitch(Pitch),
    /// One paragraph from a gloss tree.
    ///
    /// This is the only panel element that can hold more than one style.
    /// It is also the only element that can have a hit target inside its text.
    Gloss(Flow),
    /// One table from a gloss tree.
    ///
    /// This is a grid, not a run.
    /// [`Pass::table`] measures and places its cells.
    /// Of all elements that this module builds, only a table produces more than one [`SceneElem`].
    ///
    /// [`Pass::table`]: super::pass::Pass::table
    Table(Grid),
    /// One boxed block from a gloss tree.
    ///
    /// This is a container, not a run.
    /// Its box surrounds every paragraph inside it.
    /// [`Pass::boxed`] measures everything inside the box at the width that the box leaves free.
    ///
    /// [`Pass::boxed`]: super::pass::Pass::boxed
    Boxed(Boxed),
    /// A control that navigates back in history.
    BackButton(Line),
}

/// One "See also" headword.
pub(super) struct SideEntry {
    pub(super) idx: usize,
    pub(super) text: String,
    pub(super) color: Rgb,
}

/// Content of `p` in draw order.
///
/// **This is the only place that reads the render settings.**
/// [`RenderSettings`] supplies four facts here.
/// This function resolves them into two values for the gloss walk:
/// the list layout and a [`Render`] record.
/// Code after this point does not read a setting again.
/// This design uses a decision table, not a cascade of style rules.
/// Yomitan drives the same small set of root attributes into fixed CSS rules.
/// It does not offer a cascade.
pub(super) fn build_elements(
    p: &Presentation,
    theme: &Theme,
    show_back: bool,
    side_panel: bool,
    render: RenderSettings,
    selection: Option<&Selections>,
) -> (Vec<Elem>, Vec<SideEntry>) {
    let mut out = Vec::new();

    if show_back {
        out.push(Elem::BackButton(Line {
            text: "\u{2190} Back".to_string(),
            color: theme.dict_label_text,
            size: theme.collapsed_size,
            top_gap: 0.0,
            weight: theme.dict_label_weight,
            italic: theme.dict_label_italic,
        }));
    }

    if let Some(card) = &p.top {
        // This corner element comes before the headword and uses the same y position.
        if let Some(freq) = card.freq {
            out.push(Elem::Corner(Line {
                text: format!("freq {freq}"),
                color: theme.frequency_text,
                size: theme.frequency_size,
                top_gap: 0.0,
                weight: theme.frequency_weight,
                italic: theme.frequency_italic,
            }));
        }

        let headword = card.written.clone()
            .or_else(|| card.reading.clone())
            .unwrap_or_default();
        if !headword.is_empty() {
            out.push(Elem::Headword {
                headword: headword.clone(),
                prefix_u16: 0,
                line: Line {
                    text: headword.clone(),
                    color: theme.headword_text,
                    size: theme.headword_size,
                    top_gap: if show_back { LINE_GAP } else { 0.0 },
                    weight: theme.headword_weight,
                    italic: theme.headword_italic,
                },
            });
        }

        // The code shows this line only when the headword differs from the reading.
        // It also requires that no accent follows.
        // A Pitch row is the reading in marked kana.
        // A plain reading line would repeat the same string.
        // The marked reading contains the plain reading plus the pitch marks.
        if card.written.is_some() && card.pitch.is_empty() {
            if let Some(reading) = card.reading.as_deref().filter(|r| !r.is_empty()) {
                out.push(Elem::Text(Line {
                    text: reading.to_string(),
                    color: theme.reading_text,
                    size: theme.reading_size,
                    top_gap: LINE_GAP,
                    weight: theme.reading_weight,
                    italic: theme.reading_italic,
                }));
            }
        }

        // Pitch rows sit directly under the headword.
        // A Pitch row repeats the reading in marked kana, so no plain reading line is needed.
        // The code omits the plain reading because pitch rows replace it.
        // When no enabled Dictionary reports pitch for this reading, `card.pitch` is empty.
        // The code then draws no row.
        // An empty row would claim an accent that no Dictionary reported.
        if let Some(reading) = card.reading.as_deref().filter(|r| !r.is_empty()) {
            for row in &card.pitch {
                out.push(Elem::Pitch(pitch_line(reading, row, theme)));
            }
        }

        if !card.pos.is_empty() {
            out.push(Elem::Text(Line {
                text: card.pos.join(" · "),
                color: theme.dimmed_text,
                size: theme.dimmed_size,
                top_gap: LINE_GAP,
                weight: theme.dimmed_weight,
                italic: theme.dimmed_italic,
            }));
        }
        let mut entry_ordinal = 0u32;
        for block in &card.blocks {
            let label = Line {
                text: block.dict_name.clone(),
                color: theme.dict_label_text,
                size: theme.dict_label_size,
                top_gap: SECTION_GAP,
                weight: theme.dict_label_weight,
                italic: theme.dict_label_italic,
            };
            if selection.is_none() {
                out.push(Elem::Text(label.clone()));
            }
            // Yomitan's `<ol>` has one item for each matched term-bank row.
            // Hoshi Reader emits this list only when a Dictionary contributes more than one row.
            // Therefore, the code leaves a single row unnumbered.
            // This rule does not apply to Senses inside one row.
            // 大辞林 draws its own ①②③ marks in the tree.
            // An outer number would add a second number.
            let numbered = block.entries.len() > 1;
            for (i, entry) in block.entries.iter().enumerate() {
                let ordinal = entry_ordinal;
                entry_ordinal += 1;
                if let Some(sel) = selection {
                    out.push(Elem::Check {
                        entry: Some(ordinal),
                        coverage: entry_coverage(sel, ordinal, &entry.doc, render.roles),
                        top_gap: label.top_gap,
                    });
                    out.push(Elem::Text(label.clone()));
                }
                // An empty tag set means "same set as the row above" (see `GlossEntry`).
                if !entry.tags.is_empty() {
                    out.push(Elem::Text(Line {
                        text: entry.tags.join(" · "),
                        color: theme.dimmed_text,
                        size: theme.dimmed_size,
                        top_gap: LINE_GAP,
                        weight: theme.dimmed_weight,
                        italic: theme.dimmed_italic,
                    }));
                }
                // The panel renders the parsed tree, not a plain-text copy.
                // `GlossEntry::glosses` is a separate plain-text view.
                // The Anki plain-text field and the collapsed summary still need that view.
                // The design rejects a third independent view of one tree.
                let assets = Assets { dict_id: block.dict_id, sizes: &entry.media };
                let mut pieces = paragraphs(
                    &entry.doc,
                    theme,
                    LINE_GAP,
                    render.stack_items,
                    assets,
                    Render {
                        roles: render.roles,
                        styling: render.styling,
                        images: render.images,
                    },
                );
                if pieces.is_empty() {
                    continue;
                }
                // The code stamps each paragraph with its containing row.
                // The tree cannot know that row.
                // Each paragraph already carries its node path.
                // Together, the row and node path let selection target "sense 3 of 大辞林".
                for piece in &mut pieces {
                    piece.stamp(block.dict_id, entry.entry_id, ordinal);
                }
                if numbered {
                    // When a row starts with a table, the code puts the number above the table.
                    // The number appears on a separate line.
                    // The row keeps its number.
                    // Every other row adds the number to the paragraph already present.
                    // The geometry goldens pin this behavior.
                    if !matches!(pieces.first(), Some(Piece::Flow(_))) {
                        pieces.insert(
                            0,
                            Piece::Flow(Flow {
                                top_gap: LINE_GAP,
                                dict_id: block.dict_id,
                                entry_id: entry.entry_id,
                                entry: ordinal,
                                ..Flow::default()
                            }),
                        );
                    }
                    if let Some(Piece::Flow(flow)) = pieces.first_mut() {
                        flow.number(i + 1, Inline::body(theme));
                    }
                }
                out.extend(pieces.into_iter().map(|piece| match piece {
                    Piece::Flow(flow) => Elem::Gloss(flow),
                    Piece::Table(grid) => Elem::Table(grid),
                    Piece::Boxed(boxed) => Elem::Boxed(boxed),
                }));
            }
        }
    }

    let mut side = Vec::new();

    if !p.collapsed.is_empty() {
        if side_panel {
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                if head.is_empty() { continue; }
                side.push(SideEntry {
                    idx: i,
                    text: head,
                    color: theme.collapsed_text,
                });
            }
        } else {
            out.push(Elem::Separator { top_gap: SEPARATOR_MARGIN });
            for (i, row) in p.collapsed.iter().enumerate() {
                let head = related_inline_head(row.written.as_deref(), row.reading.as_deref());
                let text = if head.is_empty() {
                    row.summary.clone()
                } else {
                    format!("{head} \u{2014} {}", row.summary)
                };
                let top_gap = if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP };
                if let Some(sel) = selection {
                    let coverage = p
                        .all_cards
                        .get(i + 1)
                        .map_or(Coverage::None, |card| {
                            sel.coverage_of_card(i + 1, &card_extents(card, render.roles))
                        });
                    out.push(Elem::Check { entry: None, coverage, top_gap });
                }
                out.push(Elem::Collapsed(i, Line {
                    text,
                    color: theme.collapsed_text,
                    size: theme.collapsed_size,
                    top_gap,
                    weight: theme.collapsed_weight,
                    italic: theme.collapsed_italic,
                }));
            }
        }
    }

    (out, side)
}

fn entry_coverage(
    selection: &Selections,
    entry: u32,
    doc: &crate::dict::gloss::GlossDoc,
    roles: RoleFilter,
) -> Coverage {
    extent(doc, roles)
        .map_or(Coverage::None, |range| {
            selection.card(0).map_or(Coverage::None, |card| card.coverage(entry, range))
        })
}

fn card_extents(card: &Card, roles: RoleFilter) -> Vec<DocRange> {
    card.blocks
        .iter()
        .flat_map(|block| block.entries.iter())
        .map(|entry| extent(&entry.doc, roles).unwrap_or(DocRange {
            start: DocAddr::START,
            end: DocAddr::START,
        }))
        .collect()
}

/// One inline related row's headword in the form that the panel reads.
///
/// A related Entry is a different term.
/// Its reading identifies the term as much as its written form.
/// For example, 猫 and 描 differ in their kana.
/// The side panel shows only the headword because its column has no room for both fields.
/// An Entry with only kana would otherwise print the same string twice.
fn related_inline_head(written: Option<&str>, reading: Option<&str>) -> String {
    match (
        written.filter(|s| !s.is_empty()),
        reading.filter(|s| !s.is_empty()),
    ) {
        (Some(w), Some(r)) if w != r => format!("{w}\u{3010}{r}\u{3011}"),
        (Some(w), _) => w.to_string(),
        (None, Some(r)) => r.to_string(),
        (None, None) => String::new(),
    }
}

/// One accent in the form that the panel draws.
/// It contains marked kana and the Dictionaries that report the accent.
///
/// This function uses the reading, not the headword.
/// A Pitch pattern applies to one reading, and its moras receive the marks.
/// [`marked_morae`] computes the mora arithmetic, so this function does not repeat it.
/// The mined note's HTML field renders the same result with the same function.
/// The two views of one accent therefore cannot diverge.
fn pitch_line(reading: &str, row: &PitchRow, theme: &Theme) -> Pitch {
    Pitch {
        reading: Line {
            text: reading.to_string(),
            color: theme.reading_text,
            size: theme.reading_size,
            top_gap: LINE_GAP,
            weight: theme.reading_weight,
            italic: theme.reading_italic,
        },
        sources: Line {
            text: format!("{PITCH_SOURCE_GAP}{}", row.dicts.join(" \u{b7} ")),
            color: theme.dimmed_text,
            size: theme.dimmed_size,
            top_gap: 0.0,
            weight: theme.dimmed_weight,
            italic: theme.dimmed_italic,
        },
        morae: marked_morae(reading, &row.accent.position)
            .into_iter()
            .map(|marked| Marked {
                at: marked.mora.at,
                units: marked.mora.units,
                high: marked.high,
                fall: marked.fall,
            })
            .collect(),
    }
}

/// Label for the Anki button.
///
/// `None` means that the panel shows no button.
pub fn anki_button_label(
    p: &Presentation,
    theme: &Theme,
    anki: &AnkiPopupState,
) -> Option<(String, Rgb)> {
    if !anki.enabled { return None; }
    if !anki.connected { return None; }
    let expr = p.top.as_ref()
        .and_then(|c| c.written.as_deref().or(c.reading.as_deref()))
        .unwrap_or("");
    let (text, color) = if anki.checking {
        ("Checking\u{2026}", theme.dimmed_text)
    } else if anki.adding {
        ("Adding\u{2026}", theme.dimmed_text)
    } else if anki.failed {
        ("\u{2717} Failed to add", theme.dimmed_text)
    } else if anki.added.contains(expr) {
        ("\u{2713} Added", theme.dimmed_text)
    } else if anki.dupes.contains(expr) {
        ("\u{ff0b} Add to Anki (duplicate)", theme.dict_label_text)
    } else {
        ("\u{ff0b} Add to Anki", theme.dict_label_text)
    };
    Some((text.to_string(), color))
}
