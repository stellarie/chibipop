//! The popup's measured scene (ADR-0004).
//!
//! Layout lives here, not in the bins: element construction, wrapping,
//! line stacking, the side panel, scrollbar geometry and hit rects.
//! Bins measure text through `TextMeasure` and paint the runs the scene
//! hands them; nothing else about the panel is theirs.
//!
//! One pixel space, and no scale factor: core never sees one. Windows
//! hands in DIPs because its Direct2D target carries the DPI, Linux
//! hands in device pixels; either way the conversion is the bin's
//! (ADR-0004 - physical pixels stay authoritative, logical geometry is
//! derived).
//!
//! # The submodules, and the one reason each has to change
//!
//! Private organisation and not a seam: ADR-0004 and ADR-0013 give the
//! inline pass exactly one implementation and no trait, so every public
//! path is re-exported below and no caller anywhere names a submodule.
//! The cut is by reason to change, so a census finding, a golden field or
//! a table rule each lands in one file:
//!
//! | module | one reason to change |
//! |---|---|
//! | `measure` | the seam's contract with a platform text engine |
//! | `scene` | what a bin paints and a geometry snapshot pins |
//! | `style` | what one CSS declaration means to this renderer |
//! | `link` | what following a glossary link does |
//! | `flow` | what a paragraph carries from the walk to the measure |
//! | `gloss` | what a `GlossDoc` subtree becomes |
//! | `ruby` | how a reading buys its slot, and where it sits in it |
//! | `marker` | what a list draws beside its items, and where |
//! | `image` | how an inline asset is sized and given room |
//! | `pill` | what an inline box's own edges do to its line |
//! | `pass` | how blocks stack, and how a box closes around them |
//! | `table` | HTML's and CSS's table rules |
//! | `chrome` | what the popup shows of an entry |
//!
//! Named in prose rather than linked, because each is private: a public
//! doc comment linking a private module is a promise this module does not
//! make.
//!
//! What stays here is the face: [`scene()`], the request it takes, and the
//! scrollbar arithmetic a bin asks for by itself.

mod chrome;
mod flow;
mod gloss;
mod image;
mod link;
mod marker;
mod measure;
mod pass;
mod pill;
mod ruby;
mod scene;
mod style;
mod table;

#[cfg(test)]
mod tests;

pub use chrome::{anki_button_label, RenderSettings};
pub use measure::{
    GlyphBox, LineBox, MeasureError, MeasureRun, Measured, Metrics, SpanBox, StyledSpan,
    TextMeasure,
};
pub use scene::{
    Align, AnkiSlot, Appearance, ElemBox, ElemKind, ElemSpan, GlossOrigin, HitTarget, MarkerBox,
    Painted, PaintedRow, PopupScene, Rgb, RubyBox, SceneElem, SceneImage, SceneRect, SideRow,
    SidePanel, Tint,
};
pub use style::{BorderStyle, BoxStyle, Edges};

use crate::controller::HitAction;
use crate::present::{AnkiPopupState, Presentation};
use crate::ui::theme::{Theme, SCROLLBAR_MIN_THUMB};
use chrome::{
    build_elements, is_kanji, measure_line, one_span, side_panel, span, text_elem, Elem,
    CORNER_GAP, SEPARATOR_THICKNESS, SIDE_GAP, SIDE_PANEL_W,
};
use measure::measure_text;
use pass::Pass;
use style::REGULAR_WEIGHT;

/// One popup's whole layout input.
pub struct SceneRequest<'a> {
    pub presentation: &'a Presentation,
    pub theme: &'a Theme,
    /// The width to fill.
    pub max_w: f32,
    /// The tallest the panel may get.
    pub max_h: f32,
    pub show_back: bool,
    pub side_panel: bool,
    /// How much of an entry to show.
    pub render: RenderSettings,
    /// `Some` reserves the Anki slot.
    pub anki: Option<&'a AnkiPopupState>,
}

/// Lays out one popup.
pub fn scene(
    req: &SceneRequest<'_>,
    m: &mut dyn TextMeasure,
) -> Result<PopupScene, MeasureError> {
    let theme = req.theme;
    let font = theme.font_name.as_str();
    let pad = theme.padding as f32;
    let origin = pad;

    let (elems, entries) =
        build_elements(req.presentation, theme, req.show_back, req.side_panel, req.render);
    let has_side = !entries.is_empty();
    let side_extra = if has_side {
        SIDE_GAP + SEPARATOR_THICKNESS + SIDE_GAP + SIDE_PANEL_W
    } else {
        0.0
    };
    let content_w = (req.max_w - 2.0 * pad - side_extra).max(0.0);

    let mut y = 0.0f32;
    let mut reserved_w = 0.0f32;
    let mut probes = Vec::new();
    // Every buffer the gloss walk
    // reuses, and the scene it is
    // filling. Bundled because a
    // table lays its cells out
    // through the same paragraph
    // pass the panel's own stream
    // uses, so the pass has to be
    // callable from two places
    // without threading eight
    // arguments through both.
    let mut pass = Pass {
        font,
        theme,
        measured: Measured::default(),
        run: Vec::new(),
        cover: Vec::new(),
        out: Vec::with_capacity(elems.len()),
        hits: Vec::new(),
    };

    for elem in &elems {
        let advance = match elem {
            Elem::Separator { top_gap } => {
                let h = theme.separator_height;
                y += top_gap;
                pass.out.push(SceneElem {
                    kind: ElemKind::Separator,
                    text: String::new(),
                    color: theme.separator,
                    font_size: 0.0,
                    weight: REGULAR_WEIGHT,
                    italic: false,
                    top_gap: *top_gap,
                    wrap_w: content_w,
                    align: Align::Leading,
                    pen: (origin, origin + y),
                    rect: SceneRect { x: origin, y: origin + y, w: content_w, h },
                    lines: 0,
                    advance: h,
                    spans: Vec::new(),
                    ruby: Vec::new(),
                    marker: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
                    image: None,
                });
                h
            }
            Elem::Corner(line) => {
                // Trailing-aligned: the box
                // hugs the right edge, and
                // the run is measured
                // pre-alignment, so x comes
                // from the width.
                let met = measure_line(m, font, line, content_w, &mut pass.measured)?;
                pass.out.push(SceneElem {
                    kind: ElemKind::Corner,
                    text: line.text.clone(),
                    color: line.color,
                    font_size: line.size,
                    weight: line.weight,
                    italic: line.italic,
                    top_gap: line.top_gap,
                    wrap_w: content_w,
                    align: Align::Trailing,
                    pen: (origin, origin + y),
                    rect: SceneRect {
                        x: origin + content_w - met.w,
                        y: origin + y,
                        w: met.w,
                        h: met.h,
                    },
                    lines: met.lines,
                    advance: 0.0,
                    spans: one_span(line),
                    ruby: Vec::new(),
                    marker: Vec::new(),
                    block_box: None,
                    inline_boxes: Vec::new(),
                    origin: None,
                    image: None,
                });
                reserved_w = met.w + CORNER_GAP;
                0.0
            }
            Elem::Text(line) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Text, line, &met, origin, y, avail_w));
                h
            }
            Elem::Collapsed(idx, line) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Collapsed, line, &met, origin, y, avail_w));
                pass.hits.push(HitTarget {
                    x: None,
                    y: origin + y,
                    w: None,
                    h,
                    action: HitAction::ExpandEntry(*idx),
                });
                h
            }
            Elem::Headword { headword, prefix_u16, line } => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                let met = measure_line(m, font, line, avail_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::Headword, line, &met, origin, y, avail_w));

                let mut at = Vec::new();
                let mut chars = Vec::new();
                let mut u16_pos = *prefix_u16 as u32;
                for ch in headword.chars() {
                    if is_kanji(ch) {
                        at.push(u16_pos);
                        chars.push(ch);
                    }
                    u16_pos += ch.len_utf16() as u32;
                }
                if !at.is_empty() {
                    probes.clear();
                    let spans = [span(font, line)];
                    m.caret_boxes(
                        MeasureRun { spans: &spans, max_w: avail_w },
                        &at,
                        &mut probes,
                    )?;
                    for (ch, b) in chars.iter().zip(probes.iter()) {
                        pass.hits.push(HitTarget {
                            x: Some(origin + b.x),
                            y: origin + y + b.y,
                            w: Some(b.w),
                            h: b.h,
                            action: HitAction::DrillDown(ch.to_string()),
                        });
                    }
                }
                h
            }
            Elem::Gloss(flow) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                pass.gloss(m, flow, (origin, origin + y), avail_w)?.1
            }
            Elem::Table(grid) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                pass.table(m, grid, (origin, origin + y), avail_w)?.1
            }
            Elem::Boxed(boxed) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                pass.boxed(m, boxed, (origin, origin + y), avail_w)?.1
            }
            Elem::BackButton(line) => {
                let met = measure_line(m, font, line, content_w, &mut pass.measured)?;
                let h = met.h;
                y += line.top_gap;
                pass.out.push(text_elem(ElemKind::BackButton, line, &met, origin, y, content_w));
                pass.hits.push(HitTarget {
                    x: None,
                    y: origin + y,
                    w: None,
                    h,
                    action: HitAction::Back,
                });
                h
            }
        };
        y += advance;
    }

    let used_h = y;

    let side = if has_side {
        Some(side_panel(&entries, theme, origin, content_w, m, &mut pass.measured)?)
    } else {
        None
    };

    let side_h = side.as_ref().map_or(0.0, |s| s.height);
    let body_h = used_h.max(side_h);
    let content_h = body_h.ceil() + 2.0 * pad;
    let view_h = content_h.min(req.max_h);
    let panel_w = if has_side {
        Some(content_w + side_extra + 2.0 * pad)
    } else {
        None
    };

    let anki = match req
        .anki
        .and_then(|a| anki_button_label(req.presentation, theme, a))
    {
        Some((label, color)) => {
            let w = panel_w.unwrap_or(req.max_w);
            let met = measure_text(
                m,
                StyledSpan {
                    text: &label,
                    font,
                    size: theme.collapsed_size,
                    weight: theme.collapsed_weight,
                    italic: theme.collapsed_italic,
                    color,
                },
                w,
                &mut pass.measured,
            )?;
            Some(AnkiSlot {
                label,
                color,
                rect: SceneRect { x: 0.0, y: view_h, w, h: met.h },
            })
        }
        None => None,
    };

    Ok(PopupScene {
        origin,
        content_w,
        elems: pass.out,
        hits: pass.hits,
        side,
        anki,
        used_h,
        content_h,
        view_h,
        panel_w,
    })
}


/// The width the next stacking element gets, and the corner's claim spent.
///
/// Six of the panel's eight element kinds start here, and the reservation
/// is why: the frequency corner advances no y and hangs at the right edge,
/// so it narrows the *one* element after it ([`CORNER_GAP`]) and nothing
/// further down the panel. Reading the reservation therefore has to clear
/// it, and a seventh arm that read it without clearing it would silently
/// narrow every block below the corner instead of one.
///
/// A free function taking `&mut f32` rather than a closure over
/// `reserved_w`: the corner's own arm writes the reservation, and a
/// closure holding it borrowed for the whole loop would forbid that.
fn take_avail(content_w: f32, reserved_w: &mut f32) -> f32 {
    let avail_w = (content_w - *reserved_w).max(1.0);
    *reserved_w = 0.0;
    avail_w
}

/// Overflow past the view, or 0.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}

/// The thumb as `(top, height)`.
///
/// Floored, and kept in track.
pub fn scrollbar_thumb(
    track_h: i32,
    content_h: i32,
    view_h: i32,
    scroll: i32,
) -> Option<(i32, i32)> {
    let span = max_scroll(content_h, view_h);
    if span == 0 || track_h <= 0 || content_h <= 0 {
        return None;
    }
    let ideal = (i64::from(track_h) * i64::from(view_h) / i64::from(content_h)) as i32;
    let thumb_h = ideal.clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
    let travel = track_h - thumb_h;
    let at = scroll.clamp(0, span);
    let top = (i64::from(travel) * i64::from(at) / i64::from(span)) as i32;
    Some((top, thumb_h))
}
