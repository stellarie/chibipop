//! This module defines the measurement seam between core and a platform text
//! engine.
//!
//! **One reason to change:** Change this module when the [`TextMeasure`] contract
//! changes. The contract has no popup concept. It covers styled spans, a wrap
//! width, and line and span geometry. A platform bin that implements
//! [`TextMeasure`] reads this file and no other.
//!
//! This seam only measures text. Other layout modules can test with fixed metrics.
//! No type in this module can paint.

use std::fmt;
use super::scene::Rgb;

/// One styled span.
///
/// This is the finest unit that the seam addresses.
/// The bin's paint walk uses the color in each span.
/// The paint walk uses the same spans. The measurer does not read color.
/// Geometry does not depend on color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledSpan<'a> {
    pub text: &'a str,
    /// Font family name from the Theme.
    pub font: &'a str,
    pub size: f32,
    /// DirectWrite text weight from 100 to 900.
    pub weight: u16,
    pub italic: bool,
    pub color: Rgb,
}

/// One run to measure.
///
/// The measurer wraps all its spans as one paragraph.
/// A span boundary does not create a line boundary.
/// Bold and normal spans can share one wrapped line.
#[derive(Debug, Clone, Copy)]
pub struct MeasureRun<'a> {
    /// Span order in the text.
    pub spans: &'a [StyledSpan<'a>],
    /// Maximum wrap width.
    ///
    /// If a measurer cannot wrap at zero, it clamps the width itself.
    /// The scene reports the width that it requested.
    pub max_w: f32,
}

/// The measured values for one wrapped run.
///
/// This is the text engine's aggregate for the whole run.
/// The block pass stacks this aggregate, and the geometry goldens pin it.
/// [`Measured`] carries the line and span detail.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    /// Width of the widest line.
    pub w: f32,
    /// Combined height of all lines.
    pub h: f32,
    /// Number of wrapped lines.
    pub lines: u32,
}

/// Geometry for one wrapped line.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LineBox {
    /// Top edge relative to the run.
    pub y: f32,
    /// Width of the ink.
    pub w: f32,
    /// Distance from this top edge to the next line's top edge.
    ///
    /// A line is as tall as its tallest span.
    /// Mixed styles therefore need per-line height instead of
    /// `lines × size × LINE_HEIGHT` arithmetic.
    pub h: f32,
    /// Baseline offset below `y`.
    ///
    /// The values `{ w, h, lines }` cannot provide this offset.
    /// A superscript, a subscript, and a gaiji image at text size use this
    /// baseline. Without it, layout can only guess their positions.
    pub baseline: f32,
}

/// One piece of a span on one line.
///
/// A wrapped span has one box for each line that it touches.
/// The boxes appear in line order, then span order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpanBox {
    /// Span index in the run.
    pub span: u32,
    /// Line index in `Measured::lines`.
    pub line: u32,
    /// Leading edge relative to the run.
    pub x: f32,
    /// Advance across the line.
    pub w: f32,
    /// Line advance for this span alone.
    ///
    /// The line height is at least this value.
    /// A line uses the tallest span height.
    /// A half-size superscript does not reduce the line height.
    /// This value shows how much shorter the span is than its line.
    pub h: f32,
}

/// Results for one measured run.
///
/// The caller supplies and reuses this buffer.
/// The layout pass measures every element in a panel.
/// The inline pass measures one paragraph per block.
/// One buffer serves both passes and avoids two allocations per element per
/// frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measured {
    /// Box for the whole run.
    pub metrics: Metrics,
    /// Wrapped lines from top to bottom.
    pub lines: Vec<LineBox>,
    /// One box for each span piece on each line.
    pub spans: Vec<SpanBox>,
}

impl Measured {
    /// Clear the results before the next run.
    ///
    /// Keep vector capacity so the caller can reuse this buffer.
    pub fn clear(&mut self) {
        self.metrics = Metrics::default();
        self.lines.clear();
        self.spans.clear();
    }
}

/// One caret box inside a run.
///
/// Coordinates are relative to the run and use its layout box.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlyphBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// An error from the text engine for one run.
///
/// Layout cannot interpret a platform error. It can only stop the layout pass.
/// The platform bin adds its own error context before it returns the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureError {
    /// Text from the engine.
    pub what: String,
}

impl MeasureError {
    pub fn new(what: impl Into<String>) -> MeasureError {
        MeasureError { what: what.into() }
    }
}

impl fmt::Display for MeasureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "measuring text failed: {}", self.what)
    }
}

impl std::error::Error for MeasureError {}

/// The measure-only text engine contract.
///
/// This contract wraps styled spans at a width and reports line and span geometry.
/// It never paints. The scene stores positioned runs as plain data.
/// Layout can therefore use fixed metrics in tests.
pub trait TextMeasure {
    /// Wrap and measure `run`.
    ///
    /// The implementation empties `out` first, so one buffer can hold a whole
    /// panel.
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> Result<(), MeasureError>;

    /// Return caret boxes inside a run.
    ///
    /// The UTF-16 offsets in `at` run end to end across the concatenated spans.
    /// Write one box to `out` for each offset, in the same order.
    /// Per-character hit targets need shaped geometry. Only the measurer has
    /// that geometry.
    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError>;

    /// Return the UTF-16 offset nearest to a run-relative point.
    ///
    /// The offset uses UTF-16 units across all spans.
    /// A point above the first line returns zero.
    /// A point below the last line returns the run length.
    /// A point past a line end returns that line's end.
    /// The result can equal the run length.
    ///
    /// This belongs to the seam because DirectWrite `HitTestPoint` and cosmic-text
    /// `Buffer::hit` own the shaped geometry.
    /// A check of every caret costs O(n) for each hover.
    fn hit_offset(
        &mut self,
        run: MeasureRun<'_>,
        x: f32,
        y: f32,
    ) -> Result<u32, MeasureError>;
}

/// Measure one styled span and return its aggregate `Metrics`.
///
/// The block pass stacks whole elements and does not inspect line details.
/// This function keeps the aggregate and discards per-line and per-span details.
/// Reuse `scratch` to avoid an allocation for each element per frame.
pub(super) fn measure_text(
    m: &mut dyn TextMeasure,
    span: StyledSpan<'_>,
    max_w: f32,
    scratch: &mut Measured,
) -> Result<Metrics, MeasureError> {
    let spans = [span];
    m.measure(MeasureRun { spans: &spans, max_w }, scratch)?;
    Ok(scratch.metrics)
}
