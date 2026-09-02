//! The module builds the measured `PopupScene` for one popup.
//!
//! This module owns element construction, text wrap, line stacking, the side
//! panel, scrollbar geometry, and hit rectangles.
//! The platform bins measure text through [`TextMeasure`] and paint the runs
//! that the scene provides. They do not own other panel behavior.
//!
//! Core uses one pixel space. It never sees a scale factor.
//! Windows passes DIPs because its Direct2D target carries the DPI.
//! Linux passes device pixels. Each bin does its own conversion.
//! Physical pixels remain authoritative. Logical geometry derives from them.
//!
//! # Submodules and one reason to change for each
//!
//! These private modules do not form a seam. The inline layout pass has one
//! implementation and no trait.
//! The public paths re-export the needed types below. Callers do not name a
//! private module.
//! Each module has one reason to change. A census finding, a golden field, and
//! a table rule each belong in one file:
//!
//! | module | reason to change |
//! |---|---|
//! | `measure` | the contract with a platform text engine |
//! | `scene` | the scene that a bin paints and a geometry snapshot pins |
//! | `style` | the meaning of one CSS declaration |
//! | `link` | the action for a glossary link |
//! | `flow` | paragraph data from the walk to the measure |
//! | `gloss` | the result of a `GlossDoc` subtree |
//! | `ruby` | the slot and position for a reading |
//! | `marker` | the marker beside list items and its position |
//! | `image` | the size and space for an inline asset |
//! | `pill` | the effect of inline box edges on a line |
//! | `pass` | block order and box boundaries |
//! | `table` | the HTML and CSS table rules |
//! | `chrome` | the popup content for an `Entry` |
//!
//! The table names private modules in prose, not with links.
//! A public doc comment that links a private module promises an interface that
//! this module does not provide.
//!
//! The public surface is [`scene()`], its request, and the scrollbar arithmetic
//! that a bin uses.

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
    build_elements, is_kanji, measure_line, one_span, pitch_elem, side_panel, span, text_elem,
    Elem, CORNER_GAP, SEPARATOR_THICKNESS, SIDE_GAP, SIDE_PANEL_W,
};
use measure::measure_text;
use pass::Pass;
use style::REGULAR_WEIGHT;

/// Input for the complete layout of one popup.
pub struct SceneRequest<'a> {
    pub presentation: &'a Presentation,
    pub theme: &'a Theme,
    /// Maximum popup width.
    pub max_w: f32,
    /// Maximum popup height.
    pub max_h: f32,
    pub show_back: bool,
    pub side_panel: bool,
    /// Render settings control how entry content appears.
    pub render: RenderSettings,
    /// A `Some` value reserves the Anki slot.
    pub anki: Option<&'a AnkiPopupState>,
}

/// Build the measured scene for one popup.
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
    // Keep the gloss buffers and the scene in one `Pass`.
    // A table sends each cell through the panel's paragraph pass.
    // One `Pass` gives both callers the same buffers without eight arguments.
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
                // The box uses trailing alignment and touches the right edge.
                // The run is measured before alignment, so its width determines the x coordinate.
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
            Elem::Pitch(pitch) => {
                let avail_w = take_avail(content_w, &mut reserved_w);
                y += pitch.reading.top_gap;
                let elem = pitch_elem(
                    m,
                    font,
                    pitch,
                    (origin, origin + y),
                    avail_w,
                    &mut pass.measured,
                    &mut probes,
                )?;
                let h = elem.advance;
                pass.out.push(elem);
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

/// Return the width for the next element and clear the corner reservation.
///
/// A corner element stays at the trailing edge and does not advance `y`.
/// Its width plus [`CORNER_GAP`] reduces the available width for the next
/// element that uses this function.
/// This function clears the reservation after it reads it.
/// Without the clear, later elements would keep the reduced width.
///
/// Use a free function with `&mut f32`, not a closure over `reserved_w`.
/// The corner arm writes the reservation.
/// A closure that holds the borrow through the loop would block that write.
fn take_avail(content_w: f32, reserved_w: &mut f32) -> f32 {
    let avail_w = (content_w - *reserved_w).max(1.0);
    *reserved_w = 0.0;
    avail_w
}
/// Return content overflow past the view, or zero.
pub fn max_scroll(content_h: i32, view_h: i32) -> i32 {
    (content_h - view_h).max(0)
}
/// Return the thumb as `(top, height)`.
///
/// Integer division floors the height.
/// Clamping keeps the thumb inside the track.
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
