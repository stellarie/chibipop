//! Selection geometry for the measured popup scene.
//!
//! This module keeps address mapping beside the two operations that need the
//! same source table: painting selection highlights and resolving text hits.
//! The scene owns geometry, while the CardSelection owns document ranges.

use crate::dict::gloss::{DocAddr, NodePath};
use crate::select::{CardSelection, TextAddr};

use super::measure::{MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure};
use super::scene::{ElemKind, PopupScene, SceneElem, SceneRect};

/// Build one highlight rectangle for each selected line in each sourced gloss.
///
/// Selection ranges use document addresses, so this function first intersects
/// them with each element source and then asks the measurer for caret geometry.
/// It never highlights panel chrome or source-free spacer text.
pub(super) fn highlights(
    scene: &PopupScene,
    selection: &CardSelection,
    font: &str,
    m: &mut dyn TextMeasure,
) -> Result<Vec<SceneRect>, MeasureError> {
    let mut out = Vec::new();
    for elem in &scene.elems {
        if elem.kind != ElemKind::Text || elem.sources.is_empty() {
            continue;
        }
        let Some(origin) = elem.origin else { continue };
        let ranges = source_ranges(elem, selection, origin.entry);
        if ranges.is_empty() {
            continue;
        }
        let spans: Vec<StyledSpan<'_>> = elem.styled_spans(font).collect();
        let run = MeasureRun { spans: &spans, max_w: elem.wrap_w };
        let mut measured = Measured::default();
        m.measure(run, &mut measured)?;
        if measured.lines.is_empty() {
            continue;
        }
        let mut offsets = Vec::with_capacity(ranges.len() * 2);
        for &(start, end) in &ranges {
            offsets.push(utf16_offset(&elem.text, start));
            offsets.push(utf16_offset(&elem.text, end));
        }
        let mut carets = Vec::with_capacity(offsets.len());
        m.caret_boxes(run, &offsets, &mut carets)?;
        for [start, end] in carets.as_chunks::<2>().0 {
            out.extend(range_rects(elem, &measured, *start, *end));
        }
    }
    Ok(out)
}

impl PopupScene {
    /// Resolve a popup-local point to the nearest sourced document address.
    ///
    /// A point in a paragraph gap uses the nearest sourced element. A point in
    /// a source-free run uses the nearest source boundary in that element.
    pub fn text_hit(
        &self,
        local: (f32, f32),
        scroll: f32,
        font: &str,
        m: &mut dyn TextMeasure,
    ) -> Result<Option<TextAddr>, MeasureError> {
        let mut chosen: Option<&SceneElem> = None;
        let mut nearest: Option<(f32, &SceneElem)> = None;
        for elem in &self.elems {
            if elem.sources.is_empty() {
                continue;
            }
            let top = elem.pen.1 - scroll;
            let bottom = top + elem.rect.h;
            if local.1 >= top && local.1 < bottom {
                chosen = Some(elem);
                break;
            }
            let distance = if local.1 < top { top - local.1 } else { local.1 - bottom };
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, elem));
            }
        }
        let Some(elem) = chosen.or_else(|| nearest.map(|(_, elem)| elem)) else {
            return Ok(None);
        };
        let Some(origin) = elem.origin else { return Ok(None) };
        let spans: Vec<StyledSpan<'_>> = elem.styled_spans(font).collect();
        let offset = m.hit_offset(
            MeasureRun { spans: &spans, max_w: elem.wrap_w },
            local.0 - elem.pen.0,
            local.1 - (elem.pen.1 - scroll),
        )?;
        let byte = byte_offset(&elem.text, offset);
        Ok(Some(TextAddr { entry: origin.entry, addr: source_addr(elem, byte) }))
    }
}

fn source_ranges(elem: &SceneElem, selection: &CardSelection, entry: u32) -> Vec<(u32, u32)> {
    let selected = selection.entry_ranges(entry);
    let mut ranges = Vec::new();
    for source in &elem.sources {
        let start = DocAddr { path: source.path, byte: if source.atomic { 0 } else { source.byte } };
        let end = DocAddr {
            path: source.path,
            byte: if source.atomic { 1 } else { source.byte + source.len },
        };
        for range in &selected {
            if range.end <= start || range.start >= end {
                continue;
            }
            let (from, to) = if source.atomic {
                (0, source.len)
            } else {
                let from = if range.start <= start {
                    0
                } else {
                    addr_byte(range.start, source.path, source.byte, source.len)
                };
                let to = if range.end >= end {
                    source.len
                } else {
                    addr_byte(range.end, source.path, source.byte, source.len)
                };
                (from, to)
            };
            if from < to {
                ranges.push((source.at + from, source.at + to));
            }
        }
    }
    ranges.sort_unstable_by_key(|&(start, _)| start);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.0 <= last.1 {
                last.1 = last.1.max(range.1);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}


fn addr_byte(addr: DocAddr, path: NodePath, byte: u32, len: u32) -> u32 {
    if addr.path == path {
        addr.byte.saturating_sub(byte).min(len)
    } else {
        0
    }
}
/// One highlight box per line between the `start` caret and the `end` caret.
///
/// The first line runs from the start caret to the line end, a middle line is
/// whole, and the last line runs from the line start to the end caret. A line
/// offset by alignment slack moves both carets by that slack.
fn range_rects(
    elem: &SceneElem,
    measured: &Measured,
    start: super::measure::GlyphBox,
    end: super::measure::GlyphBox,
) -> Vec<SceneRect> {
    let first = line_index(measured, start.y);
    let last = line_index(measured, end.y);
    if first >= measured.lines.len() || last >= measured.lines.len() {
        return Vec::new();
    }
    let first = first.min(last);
    let last = last.max(first);
    let mut out = Vec::with_capacity(last - first + 1);
    for line_index in first..=last {
        let line = measured.lines[line_index];
        let line_start = elem.pen.0
            + (elem.wrap_w - line.w).max(0.0) * elem.align.slack_before();
        let line_end = line_start + line.w;
        let (x0, x1) = if first == last {
            (line_start + start.x, line_start + end.x)
        } else if line_index == first {
            (line_start + start.x, line_end)
        } else if line_index == last {
            (line_start, line_start + end.x)
        } else {
            (line_start, line_end)
        };
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        if x1 > x0 {
            out.push(SceneRect { x: x0, y: elem.pen.1 + line.y, w: x1 - x0, h: line.h });
        }
    }
    out
}

fn line_index(measured: &Measured, y: f32) -> usize {
    measured
        .lines
        .iter()
        .position(|line| y >= line.y && y < line.y + line.h)
        .unwrap_or_else(|| {
            measured
                .lines
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (a.y - y)
                        .abs()
                        .partial_cmp(&(b.y - y).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map_or(0, |(index, _)| index)
        })
}

fn utf16_offset(text: &str, byte: u32) -> u32 {
    text.get(..byte as usize)
        .unwrap_or(text)
        .encode_utf16()
        .count() as u32
}

fn byte_offset(text: &str, offset: u32) -> u32 {
    if offset == 0 {
        return 0;
    }
    let mut units = 0u32;
    for (byte, ch) in text.char_indices() {
        if units >= offset {
            return byte as u32;
        }
        units += ch.len_utf16() as u32;
    }
    text.len() as u32
}

fn source_addr(elem: &SceneElem, byte: u32) -> DocAddr {
    let previous = elem
        .sources
        .iter()
        .filter(|source| source.at + source.len <= byte)
        .max_by_key(|source| source.at + source.len);
    let next = elem.sources.iter().filter(|source| source.at >= byte).min_by_key(|source| source.at);
    let source = elem
        .sources
        .iter()
        .find(|source| byte >= source.at && byte < source.at + source.len)
        .or_else(|| {
            match (previous, next) {
                (Some(previous), Some(next)) => {
                    let before = byte - (previous.at + previous.len);
                    let after = next.at - byte;
                    (before <= after).then_some(previous).or(Some(next))
                }
                (Some(previous), None) => Some(previous),
                (None, Some(next)) => Some(next),
                (None, None) => None,
            }
        })
        .unwrap_or(&elem.sources[0]);
    let relative = byte.saturating_sub(source.at).min(source.len);
    DocAddr {
        path: source.path,
        byte: if source.atomic {
            u32::from(relative >= source.len / 2)
        } else {
            source.byte + relative
        },
    }
}
