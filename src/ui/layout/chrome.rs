//! What the popup shows of an entry, and the furniture around it.
//!
//! **One reason to change:** the panel's own content - a row added or
//! dropped, a heading reworded, an affordance reserved.
//!
//! The one place the render settings are read
//! ([`build_elements`]), which is what keeps [`RenderSettings`] a decision
//! table rather than a cascade: its four facts resolve here into the two
//! things the gloss walk takes, and nothing below this point consults a
//! setting again.
//!
//! Everything here is the panel's, not a dictionary's: a chrome element
//! carries one style, so it has one span, and it gets the seam exactly the
//! request it got before the inline pass existed.

use crate::dict::gloss::RoleFilter;
use crate::present::{AnkiPopupState, Presentation};
use crate::ui::theme::Theme;
use super::flow::Flow;
use super::gloss::{paragraphs, Assets, Render};
use super::measure::{measure_text, MeasureError, Measured, Metrics, StyledSpan, TextMeasure};
use super::pass::{Boxed, Piece};
use super::scene::{Align, ElemKind, ElemSpan, Rgb, SceneElem, SceneRect, SidePanel, SideRow};
use super::style::Inline;
use super::table::Grid;

/// Gap within a block.
pub(super) const LINE_GAP: f32 = 4.0;

/// Gap before a new block.
pub(super) const SECTION_GAP: f32 = 10.0;

/// Gap beside the corner elem.
pub(super) const CORNER_GAP: f32 = 8.0;

/// Gap around the rule.
pub(super) const SEPARATOR_MARGIN: f32 = 10.0;

/// The side column's vertical rule.
///
/// Not `Theme::separator_height`: that
/// themes the horizontal rule between
/// blocks, and a height cannot set the
/// width of a vertical one.
pub(super) const SEPARATOR_THICKNESS: f32 = 1.0;

/// Fixed "See also" column width.
pub(super) const SIDE_PANEL_W: f32 = 110.0;

/// Gap before the side panel.
pub(super) const SIDE_GAP: f32 = 12.0;

/// What the popup shows of an entry,
/// as the scene builder spends it.
///
/// The spec's "render settings are a
/// decision table, not a style
/// engine": Yomitan's own
/// configurability is a small fixed
/// set of root attributes driving
/// fixed CSS rules, and this is
/// chibipop's mirror of it -
/// `popup`'s six knobs resolved to
/// the four facts the gloss walk
/// actually spends. Resolved by
/// `PopupConfig::render_settings`, so
/// the enum-to-flag mapping lives
/// once and the walk never sees a
/// config type.
///
/// Read in exactly one place
/// ([`build_elements`]), which is
/// what keeps this a table rather
/// than a cascade: nothing below that
/// point consults a setting again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderSettings {
    /// Does a glossary list stack its
    /// items?
    ///
    /// `true` gives each item its own
    /// line, marked and indented;
    /// `false` joins them into one
    /// paragraph with
    /// [`ITEM_SEPARATOR`] between
    /// them, which is the terse popup
    /// chibipop drew before it could
    /// do better. The same parameter
    /// the list pass already had
    /// ([`Paragraphs::stack_items`]) -
    /// compact mode flips it and
    /// invents nothing.
    ///
    /// [`ITEM_SEPARATOR`]: super::gloss::ITEM_SEPARATOR
    /// [`Paragraphs::stack_items`]: super::gloss::Paragraphs::stack_items
    pub stack_items: bool,
    /// Do a dictionary's own
    /// declarations apply?
    ///
    /// Off draws every entry in the
    /// theme's own font and colours.
    /// One flag for both sources by
    /// construction: inline `style`
    /// and a dictionary's own
    /// `styles.css` land in the same
    /// resolved record, and
    /// [`Paragraphs::declarations`] is
    /// the single gate over it.
    ///
    /// [`Paragraphs::declarations`]: super::gloss::Paragraphs::declarations
    pub styling: bool,
    /// Do image assets reach the
    /// panel?
    ///
    /// Off leaves the `alt` text: an
    /// image node is a *character*
    /// far more often than an
    /// illustration, so a node cut out
    /// whole would leave a hole in a
    /// word.
    pub images: bool,
    /// Which editorial roles reach the
    /// panel.
    ///
    /// The card renderer's own
    /// vocabulary, and a separate
    /// *value* of it: the popup's
    /// answer and the card's are two
    /// filters over one enum, which is
    /// what lets a user hide examples
    /// on screen and keep them on a
    /// mined card. The card's is
    /// [`RoleFilter::CARD`] and no
    /// setting reaches it.
    pub roles: RoleFilter,
}

impl Default for RenderSettings {
    /// What a fresh install draws: a
    /// stacked list, the dictionary's
    /// own styling, its images, and
    /// its editorial matter minus the
    /// part-of-speech labels the card
    /// already prints above the
    /// glosses.
    ///
    /// The same values `PopupConfig`'s
    /// own defaults resolve to - a
    /// test pins the two together -
    /// so a geometry fixture and a
    /// fresh install draw the same
    /// panel.
    fn default() -> RenderSettings {
        RenderSettings {
            stack_items: true,
            styling: true,
            images: true,
            roles: RoleFilter::CARD,
        }
    }
}

/// The shared text-element shape.
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
        // The panel's own chrome: a
        // headword, a reading, a
        // dictionary label and a
        // collapsed row are built from
        // a `Presentation` and not from
        // a parsed tree, so none of
        // them addresses a node.
        origin: None,
        // Chrome composites no asset.
        image: None,
    }
}

/// The one span a `Line` is, as the
/// scene carries it.
///
/// Every element the panel's own
/// chrome builds has one style, so it
/// has one span - and the seam gets
/// exactly the request it got before
/// the inline pass existed.
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

/// The one span a `Line` is, as the
/// seam takes it.
///
/// Every element the panel's own
/// chrome builds carries one style; a
/// gloss paragraph is what hands the
/// seam more than one, through
/// [`Flow::styled_spans`].
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
/// One role for the whole column, the
/// collapsed one, so its rows differ
/// only in text and colour - and no
/// geometry rides on colour.
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

/// The "See also" column's geometry.
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

/// CJK ideograph check.
pub(super) fn is_kanji(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

/// One line to lay out or draw.
///
/// Rebuilt, never cached.
pub(super) struct Line {
    pub(super) text: String,
    pub(super) color: Rgb,
    pub(super) size: f32,
    /// Extra space above this line.
    pub(super) top_gap: f32,
    /// DirectWrite weight, 100-900.
    pub(super) weight: u16,
    pub(super) italic: bool,
}

pub(super) enum Elem {
    Text(Line),
    /// Right-aligned, advances no y.
    ///
    /// Steals width from the next.
    Corner(Line),
    Separator { top_gap: f32 },
    /// A clickable collapsed row.
    Collapsed(usize, Line),
    /// Per-char click targets.
    Headword {
        headword: String,
        prefix_u16: usize,
        line: Line,
    },
    /// One paragraph of a gloss tree.
    ///
    /// The only element the panel
    /// builds that can hold more than
    /// one style, and the only one
    /// that can earn a hit target
    /// inside its own text.
    Gloss(Flow),
    /// One table of a gloss tree.
    ///
    /// A grid rather than a run: its
    /// cells are measured and placed
    /// by [`Pass::table`], which is
    /// the only element that produces
    /// more than one [`SceneElem`].
    ///
    /// [`Pass::table`]: super::pass::Pass::table
    Table(Grid),
    /// One boxed block of a gloss
    /// tree.
    ///
    /// A container rather than a run:
    /// its box wraps every paragraph
    /// inside it, and everything in it
    /// is measured at the width the
    /// box left ([`Pass::boxed`]).
    ///
    /// [`Pass::boxed`]: super::pass::Pass::boxed
    Boxed(Boxed),
    /// Navigate back in history.
    BackButton(Line),
}

/// One "See also" headword.
pub(super) struct SideEntry {
    pub(super) idx: usize,
    pub(super) text: String,
    pub(super) color: Rgb,
}

/// `p`'s content, in draw order.
///
/// **The one place the render settings
/// are read.** [`RenderSettings`]'
/// four facts resolve here into the
/// two things the gloss walk takes -
/// a list's layout and a [`Render`]
/// record - and nothing below this
/// point consults a setting again.
/// That is the spec's "decision
/// table, not a style engine":
/// Yomitan drives the same handful of
/// root attributes into fixed CSS
/// rules rather than offering a
/// cascade.
pub(super) fn build_elements(
    p: &Presentation,
    theme: &Theme,
    show_back: bool,
    side_panel: bool,
    render: RenderSettings,
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
        // Before the headword: same y.
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

        // Only if the headword differs.
        if card.written.is_some() {
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

        for block in &card.blocks {
            out.push(Elem::Text(Line {
                text: block.dict_name.clone(),
                color: theme.dict_label_text,
                size: theme.dict_label_size,
                top_gap: SECTION_GAP,
                weight: theme.dict_label_weight,
                italic: theme.dict_label_italic,
            }));
            // Yomitan's `<ol>` holds one item per matched term-bank row, and
            // Hoshi Reader emits the list at all only when a dictionary
            // contributed more than one row - so one row is unnumbered. Never
            // the Senses inside a row: 大辞林 draws its own ①②③ in the tree,
            // and an outer number would double-number it.
            let numbered = block.entries.len() > 1;
            for (i, entry) in block.entries.iter().enumerate() {
                // Empty means "same set as the row above" (see `GlossEntry`).
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
                // The panel renders the parsed tree, not the plain-text
                // render of it: `GlossEntry::glosses` is what the Anki
                // plain-text field and the collapsed summary still need,
                // and a third view of one tree is the bug class this spec
                // set out to close.
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
                // The row behind every
                // paragraph of it, which the
                // tree itself cannot know.
                // With the node path each
                // paragraph already carries,
                // this is what turns "select
                // the text" into "select sense
                // 3 of 大辞林" (stories 45
                // and 46).
                for piece in &mut pieces {
                    piece.stamp(block.dict_id, entry.entry_id);
                }
                if numbered {
                    // Yomitan's number is the
                    // `<li>` marker, outside the
                    // content, so a row that opens
                    // with a table takes its
                    // number on the line above
                    // rather than losing it. Every
                    // other row prefixes the
                    // paragraph it already has,
                    // which is what the geometry
                    // goldens hold.
                    if !matches!(pieces.first(), Some(Piece::Flow(_))) {
                        pieces.insert(
                            0,
                            Piece::Flow(Flow {
                                top_gap: LINE_GAP,
                                dict_id: block.dict_id,
                                entry_id: entry.entry_id,
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
                let head = row.written.clone()
                    .or_else(|| row.reading.clone())
                    .unwrap_or_default();
                let text = if head.is_empty() {
                    row.summary.clone()
                } else {
                    format!("{head} \u{2014} {}", row.summary)
                };
                out.push(Elem::Collapsed(i, Line {
                    text,
                    color: theme.collapsed_text,
                    size: theme.collapsed_size,
                    top_gap: if i == 0 { SEPARATOR_MARGIN } else { LINE_GAP },
                    weight: theme.collapsed_weight,
                    italic: theme.collapsed_italic,
                }));
            }
        }
    }

    (out, side)
}

/// The Anki button's label.
///
/// `None` means: show no button.
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
