//! The measurement seam: what core asks a text engine for, and what comes
//! back (ADR-0004, widened by ADR-0013).
//!
//! **One reason to change:** the contract between core and a platform text
//! engine. Nothing here knows what a popup is - it is styled spans, a wrap
//! width, and the line and span geometry that answers them - so a bin
//! implementing [`TextMeasure`] reads this file and no other.
//!
//! Measure-only by construction, and that is what makes every other module
//! here testable against fixed metrics: no type in this file can paint.

use std::fmt;
use super::scene::Rgb;

/// One run of text with one style.
///
/// The finest unit the seam addresses
/// (ADR-0013). Colour rides along for
/// the bin's paint walk, which shapes
/// the same spans; no measurer reads
/// it and no geometry depends on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledSpan<'a> {
    pub text: &'a str,
    /// Family name, from the theme.
    pub font: &'a str,
    pub size: f32,
    /// DirectWrite weight, 100-900.
    pub weight: u16,
    pub italic: bool,
    pub color: Rgb,
}

/// One run to measure.
///
/// Its spans wrap as one paragraph, so
/// a span boundary is not a line
/// boundary. That is the whole of what
/// ADR-0013 widened: before it, bold
/// text and normal text could not
/// share a wrapped line.
#[derive(Debug, Clone, Copy)]
pub struct MeasureRun<'a> {
    /// In reading order.
    pub spans: &'a [StyledSpan<'a>],
    /// Wrap width. A measurer that
    /// cannot wrap at zero clamps it
    /// itself; the scene reports the
    /// width it asked for.
    pub max_w: f32,
}

/// What one wrapped run measures.
///
/// The engine's own aggregate for the
/// whole run, which is what the block
/// walk stacks and what the geometry
/// goldens pin. [`Measured`] carries
/// the detail beside it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    /// Widest line's width.
    pub w: f32,
    /// All lines' height.
    pub h: f32,
    /// Wrapped line count.
    pub lines: u32,
}

/// One wrapped line's geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LineBox {
    /// Top edge, run-relative.
    pub y: f32,
    /// Inked width.
    pub w: f32,
    /// Top edge to the next line's.
    ///
    /// As tall as the tallest span on
    /// it, which is why mixed styling
    /// ended the old `lines × size ×
    /// LINE_HEIGHT` arithmetic.
    pub h: f32,
    /// Baseline, down from `y`.
    ///
    /// The one thing `{ w, h, lines }`
    /// could never say. A superscript,
    /// a subscript and a gaiji image at
    /// text size are all positions
    /// relative to this, so without it
    /// there is no arithmetic to place
    /// them, only a guess (ADR-0013).
    pub baseline: f32,
}

/// One span's piece of one line.
///
/// A span that wraps gets one of these
/// per line it touches, in line order
/// then span order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SpanBox {
    /// Index into the run's spans.
    pub span: u32,
    /// Index into `Measured::lines`.
    pub line: u32,
    /// Leading edge, run-relative,
    /// like the line it sits on.
    pub x: f32,
    /// Advance across the line.
    pub w: f32,
    /// The line advance this span asks
    /// for on its own.
    ///
    /// Its line's `h` is at least this
    /// much: a line is as tall as its
    /// tallest span, so a half-size
    /// superscript never shrinks the
    /// line it rides on, and this is
    /// what says how much shorter than
    /// the line the span itself is.
    pub h: f32,
}

/// What one measured run is.
///
/// Handed in and refilled rather than
/// returned: the walk measures every
/// element in a panel and the inline
/// pass measures a paragraph per
/// block, so one buffer serves them
/// all instead of two allocations per
/// element per frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measured {
    /// The whole run's box.
    pub metrics: Metrics,
    /// Each wrapped line, top down.
    pub lines: Vec<LineBox>,
    /// Each span's piece of each line.
    pub spans: Vec<SpanBox>,
}

impl Measured {
    /// Empties it for the next run.
    ///
    /// Keeps the capacity: that is the
    /// point of handing it in.
    pub fn clear(&mut self) {
        self.metrics = Metrics::default();
        self.lines.clear();
        self.spans.clear();
    }
}

/// One caret's box inside a run.
///
/// Run-relative, like the layout box
/// the measurer wrapped.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlyphBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The text engine refused a run.
///
/// Opaque: layout cannot interpret a
/// platform failure, only abandon the
/// walk. The bin re-attaches its own
/// error on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureError {
    /// What the engine reported.
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

/// Text measurement, and nothing else.
///
/// Measure-only by construction: wrap
/// styled spans at a width, report
/// line and span geometry. It never
/// paints, and the scene it feeds
/// carries positioned runs as plain
/// data, so layout is testable against
/// fixed metrics (ADR-0004, amended by
/// ADR-0013).
pub trait TextMeasure {
    /// Wrap `run` and measure it.
    ///
    /// `out` is emptied first, so one
    /// buffer measures a whole panel.
    fn measure(
        &mut self,
        run: MeasureRun<'_>,
        out: &mut Measured,
    ) -> Result<(), MeasureError>;

    /// Caret boxes inside a run.
    ///
    /// `at` are UTF-16 offsets into the
    /// run's spans end to end; one box
    /// per offset is pushed to `out`,
    /// in order. Per-character hit
    /// targets need shaped geometry,
    /// which only the measurer has.
    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError>;
}

/// Measures one styled span's box.
///
/// The block walk stacks whole
/// elements and never looks inside a
/// line, so it keeps the aggregate and
/// drops the per-line and per-span
/// detail. `scratch` is what keeps
/// dropping it from costing an
/// allocation per element per frame.
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
