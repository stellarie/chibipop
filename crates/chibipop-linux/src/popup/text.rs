//! The Linux text stack. cosmic-text sits behind core's `TextMeasure`.
//! This module also holds the paint half and the startup Japanese-font
//! probe.
//!
//! **The locale is the whole feature.** cosmic-text picks a Han
//! fallback through `han_unification(locale)`. On unix, only `"ja"`
//! gives `Noto Sans CJK JP`. Every other arm gives `Noto Sans CJK SC`,
//! which draws kanji with *Chinese* glyph shapes. A stock desktop hits
//! one of those arms, because `sys_locale` hands `en-US` to
//! `FontSystem::new()`. cosmic-text reports no error and logs nothing.
//! The popup then teaches the user the wrong character forms, and this
//! product cannot ship that failure. Therefore this module builds the
//! `FontSystem` by hand and pins the locale. It never calls
//! `FontSystem::new()`.
//!
//! **One shaping path.** `measure` and `draw_run` both call [`shape`].
//! No run wraps one way and paints another. Windows earns the same
//! property from one `IDWriteTextLayout`. The twin there is
//! `Text::layout` in `chibipop-windows/src/ui/render.rs`.
//!
//! **Physical pixels only.** `popup::physical_theme` scales `size` and
//! `max_w` before they arrive. This module holds no logical-pixel
//! arithmetic.
//!
//! The twins diverge at hit-testing. DirectWrite hit-tests UTF-16
//! natively, so Windows passes core's caret offsets straight to
//! `HitTestTextPosition`. cosmic-text is UTF-8 throughout, and
//! `LayoutGlyph::start`/`end` are byte ranges into the line. Core
//! pairs its UTF-16 offsets with the kanji of a headword. Therefore
//! [`byte_offset`] converts each of those offsets into a byte offset
//! here.

use chibipop::ui::layout::{
    GlyphBox, LineBox, MeasureError, MeasureRun, Measured, Metrics, SpanBox, StyledSpan,
    TextMeasure,
};
use cosmic_text::{
    fontdb, Attrs, Buffer, Color, Family, FontSystem, LayoutRun, Metrics as CosmicMetrics,
    Shaping, Style, SwashCache, Weight, Wrap,
};
use tiny_skia::{PixmapMut, PremultipliedColorU8};

use crate::popup::{DrawRun, PanelText};

/// Line advance, as a multiple of the font size.
///
/// Core's line stacking rests on this one number. Every run's height
/// is a whole number of these advances, so blocks stack exactly. The
/// `hhea` ascent plus descent of Noto Sans CJK JP is 1.448 em with a
/// zero line gap. That value is correct but loose. Yu Gothic UI
/// through DirectWrite reaches about 1.36 em, and the Windows popup
/// uses that value. Users compare against the Windows popup, so 1.4
/// sits between the two numbers. 1.4 is tight enough to keep the
/// Linux popup no looser than the Windows one. 1.4 is also loose
/// enough to keep full-height kana and kanji clear of a descender.
/// The panel paints with no clipping. Lines next to each other would
/// otherwise touch ink.
const LINE_HEIGHT: f32 = 1.4;

/// cosmic-text asserts on a zero font size or line height. A scene
/// that asks for zero is a caller bug. That bug must not abort the
/// daemon mid-paint. The clamp also disarms NaN, because `f32::max`
/// returns the finite operand.
const MIN_SIZE: f32 = 1.0;

/// The kanji the startup probe shapes: 漢字, two of the most common
/// ideographs. If these two are tofu, every glyph the popup shows is
/// tofu.
const PROBE_TEXT: &str = "\u{6f22}\u{5b57}";

/// The package to name in the missing-font warning.
///
/// Arch calls the package `noto-fonts-cjk`. Debian and Fedora both
/// ship an alias or a virtual package under the same name. The message
/// names one concrete family, and that name covers the rest.
pub const PACKAGE: &str = "noto-fonts-cjk";

/// cosmic-text, with the locale pinned and its caches warm.
///
/// One per daemon. The constructor parses every installed face, which
/// costs the better part of a second.
pub struct TextEngine {
    fonts: FontSystem,
    /// Rasterized glyph images, keyed by face, size and subpixel
    /// offset. The popup re-shapes on every paint, and this cache makes
    /// each re-shape after the first frame free.
    swash: SwashCache,
    /// The family the config resolved to. The paint path uses it.
    /// `measure` takes its family from the run instead, because core
    /// carries the theme's name through the scene.
    family: String,
}

impl TextEngine {
    pub fn new(family: &str) -> TextEngine {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // The generic aliases (`Family::Serif` and friends) stay at
        // fontdb's defaults on purpose. Every run this engine shapes
        // names a concrete family, and the Han fallback list holds
        // concrete family names too. Nothing consults the aliases.
        //
        // Use the locale `"ja"`, and never `FontSystem::new()`. See the
        // module doc. The default arm of cosmic-text's
        // `han_unification` is Simplified Chinese, so a system locale
        // of `en-US` renders kanji with Chinese glyph variants and
        // reports no error.
        TextEngine {
            fonts: FontSystem::new_with_locale_and_db("ja".to_string(), db),
            swash: SwashCache::new(),
            family: family.to_string(),
        }
    }

    /// The family the paint path uses.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Select another family for paints, and keep the loaded font db.
    ///
    /// The engine exists *before* resolution: `resolve_font` asks the
    /// engine whether the configured family is installed. A reload can
    /// change the family again later. A second parse of every
    /// installed face for one string swap would cost the better part
    /// of a second on the daemon thread.
    pub fn set_family(&mut self, family: &str) {
        self.family.clear();
        self.family.push_str(family);
    }

    /// True when the font stack holds `family`.
    ///
    /// `chibipop::config::resolve_font` wants this closure, and the
    /// answer must be honest. A `false` turns the configured family
    /// into a visible `FontChoice::Fallback`. The fold is ASCII-only,
    /// so localized Japanese family names compare exactly. That form
    /// is what the user typed either way.
    pub fn resolvable(&self, family: &str) -> bool {
        self.fonts
            .db()
            .faces()
            .flat_map(|face| face.families.iter())
            .any(|(name, _)| name.eq_ignore_ascii_case(family))
    }

    /// What Japanese looks like on this machine.
    ///
    /// The answer rests on two independent facts. Fact one is whether
    /// kanji resolve to real glyphs at all. This method shapes them
    /// and is the only authority on tofu. Fact two is whether the
    /// family that answers is Japanese. That fact is a name question,
    /// and [`classify`] answers it.
    pub fn probe(&mut self) -> JpFonts {
        // Shape first. The shape call takes `&mut FontSystem`, `db()`
        // borrows the same `FontSystem`, and the name list borrows out
        // of the db.
        let covered = covers(&mut self.fonts, &self.family, PROBE_TEXT);
        let own = self.resolvable(&self.family);
        let mut names: Vec<&str> = self
            .fonts
            .db()
            .faces()
            .flat_map(|face| face.families.iter())
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        if own {
            // Ask about the family the popup paints with first. A
            // verdict then names that family, and not whichever
            // installed Japanese family sorts earliest.
            names.insert(0, &self.family);
        }
        classify(&names, covered)
    }

    /// The one shaping call.
    ///
    /// Measure and paint both come through here, on the same styled
    /// spans. No run wraps one way and paints another.
    ///
    /// This is an associated function, not a method. A caller can
    /// therefore hand it `&mut self.fonts` and hold `&self.family` at
    /// the same time.
    fn shape(fonts: &mut FontSystem, spans: &[StyledSpan<'_>], max_w: f32) -> Buffer {
        // The buffer's own metrics answer for a line with no glyphs on
        // it. That line is the only line the spans cannot speak for.
        let size = spans.first().map_or(MIN_SIZE, |s| s.size).max(MIN_SIZE);
        let mut buffer = Buffer::new_empty(CosmicMetrics::new(size, size * LINE_HEIGHT));
        // No height bound: shape every wrapped line, not only the
        // lines that fit a viewport. Core culls off-panel runs itself,
        // and it needs the full metrics to decide.
        //
        // DirectWrite gets the same width clamp on Windows. A measurer
        // that cannot wrap at zero clamps itself, and the scene still
        // reports the width it asked for.
        buffer.set_size(Some(max_w.max(1.0)), None);
        // Words first, then glyphs when one "word" cannot fit alone.
        // Japanese has no spaces, so the segmenter treats runs of
        // ideographs as their own words. That behaves like CJK line
        // breaking. The glyph rule keeps a long Latin headword inside
        // a narrow panel.
        buffer.set_wrap(Wrap::WordOrGlyph);
        // Call `set_rich_text`, not `set_text`. The spans wrap as one
        // paragraph, so a span boundary is not a line boundary. Bold
        // text can therefore share a line with normal text
        // (ARCHITECTURE.md#popup-and-measurement). Per-span `metrics`
        // carries each span's own size into the line height, and
        // cosmic-text takes the maximum of those sizes. The default
        // attrs carry no metrics on purpose, so a glyphless line uses
        // the buffer's metrics.
        let default = spans.first().map_or_else(Attrs::new, attrs_of);
        let styled = spans.iter().map(|s| {
            let size = s.size.max(MIN_SIZE);
            (s.text, attrs_of(s).metrics(CosmicMetrics::new(size, line_height(size))))
        });
        buffer.set_rich_text(styled, &default, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        buffer
    }
}

/// One span's shaping attributes, minus its size.
///
/// The theme sets weight and style for each role (CSS theming). fontdb
/// weights are the same 100-900 numbers that DirectWrite takes, so the
/// scene's number travels unconverted. A family with no bold or italic
/// face still shapes: fontdb picks the nearest weight it holds, and
/// cosmic-text synthesizes nothing. A missing face therefore costs the
/// emphasis, and never the run.
fn attrs_of<'a>(span: &StyledSpan<'a>) -> Attrs<'a> {
    Attrs::new()
        .family(Family::Name(span.font))
        .weight(Weight(span.weight))
        .style(if span.italic { Style::Italic } else { Style::Normal })
}

/// One coverage probe's span.
///
/// The span is regular and upright. A probe asks which face answers
/// for a character. A family's bold or italic face, when the family
/// has one, covers what its regular face covers. Color is not a
/// measurement input, so black serves as well as any other color.
fn probe_span<'a>(text: &'a str, family: &'a str, size: f32) -> StyledSpan<'a> {
    StyledSpan {
        text,
        font: family,
        size,
        weight: Weight::NORMAL.0,
        italic: false,
        color: (0, 0, 0),
    }
}

/// The line advance that a run of `size` uses to stack.
fn line_height(size: f32) -> f32 {
    size.max(MIN_SIZE) * LINE_HEIGHT
}

impl TextMeasure for TextEngine {
    /// Never `Err`.
    ///
    /// The fallible signature exists for DirectWrite's HRESULTs. A
    /// pure-Rust shaper has nothing to refuse. Missing glyphs are no
    /// failure here either: cosmic-text picks a fallback and then
    /// emits `.notdef`, which measures like any other glyph.
    /// [`TextEngine::probe`] reports tofu once at startup, and no
    /// caller reports it per run.
    fn measure(&mut self, run: MeasureRun<'_>, out: &mut Measured) -> Result<(), MeasureError> {
        out.clear();
        let buffer = TextEngine::shape(&mut self.fonts, run.spans, run.max_w);
        let mut w = 0.0f32;
        // Sum wide, and round once. Every line of a one-style run has
        // the same advance, and the seam this widening protects
        // reported `lines × advance`. Repeated `f32` addition drifts
        // one ulp below that product by the eighth line. A block stack
        // that moves is a golden that moves, so this code cannot
        // afford the drift.
        let mut h = 0.0f64;
        let mut bases = LineBases::default();
        for (i, line) in buffer.layout_runs().enumerate() {
            let base = bases.advance(run.spans, &line);
            w = w.max(line.line_w);
            h += f64::from(line.line_height);
            out.lines.push(LineBox {
                y: line.line_top,
                w: line.line_w,
                h: line.line_height,
                // `line_y` is the baseline in buffer space. A line box
                // reports the baseline from the line's own top.
                baseline: line.line_y - line.line_top,
            });
            for glyph in line.glyphs {
                let Some((s, span)) = span_at(run.spans, base + glyph.start) else {
                    continue;
                };
                let (s, i) = (s as u32, i as u32);
                // Glyphs arrive in visual order, so a span's box on
                // this line is the last box pushed for that span. That
                // box only grows.
                match out.spans.iter_mut().rev().find(|b| b.span == s && b.line == i) {
                    Some(b) => {
                        let right = (b.x + b.w).max(glyph.x + glyph.w);
                        b.x = b.x.min(glyph.x);
                        b.w = right - b.x;
                    }
                    None => out.spans.push(SpanBox {
                        span: s,
                        line: i,
                        x: glyph.x,
                        w: glyph.w,
                        h: line_height(span.size),
                    }),
                }
            }
        }
        // An empty run is one empty line, and never zero lines. Core
        // stacks the gap after the run either way.
        if out.lines.is_empty() {
            h = f64::from(line_height(run.spans.first().map_or(0.0, |s| s.size)));
            // cosmic-text centers a glyphless line's baseline inside
            // the line. The line has no ascent to hang it from.
            let h = h as f32;
            out.lines.push(LineBox { y: 0.0, w: 0.0, h, baseline: h / 2.0 });
        }
        // Stackable by construction: a whole number of line advances,
        // and never an ink box. Core's walk sums these values.
        out.metrics = Metrics { w, h: h as f32, lines: out.lines.len() as u32 };
        Ok(())
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError> {
        let buffer = TextEngine::shape(&mut self.fonts, run.spans, run.max_w);
        // One box per offset, in order. Core pairs these boxes 1:1
        // with the kanji of a headword to build per-character hit
        // targets. A skipped offset would shift every later target,
        // and nothing would report the shift.
        for &offset in at {
            out.push(caret_box(&buffer, run.spans, offset));
        }
        Ok(())
    }

    fn hit_offset(
        &mut self,
        run: MeasureRun<'_>,
        x: f32,
        y: f32,
    ) -> Result<u32, MeasureError> {
        let buffer = TextEngine::shape(&mut self.fonts, run.spans, run.max_w);
        let first_y = buffer.layout_runs().next().map(|line| line.line_top);
        let last = buffer
            .layout_runs()
            .last()
            .map(|line| (line.line_top, line.line_height));
        let total_bytes = run.spans.iter().map(|span| span.text.len()).sum();
        let total = utf16_offset(run.spans, total_bytes);
        let Some(first_y) = first_y else {
            return Ok(total);
        };
        if y < first_y {
            return Ok(0);
        }
        let Some((last_y, last_h)) = last else {
            return Ok(total);
        };
        if y >= last_y + last_h {
            return Ok(total);
        }
        let mut hit = buffer.hit(x, y);
        if hit.is_none() {
            // cosmic-text returns no cursor for a point outside its visible
            // runs. Retry inside the first or last run before giving up.
            let retry_y = y.clamp(first_y, last_y + last_h - f32::EPSILON);
            hit = buffer.hit(x, retry_y);
        }
        let Some(cursor) = hit else {
            return Ok(total);
        };

        let mut bases = LineBases::default();
        for line in buffer.layout_runs() {
            let base = bases.advance(run.spans, &line);
            if line.line_i == cursor.line {
                return Ok(utf16_offset(run.spans, base + cursor.index.min(line.text.len())));
            }
        }
        Ok(total)
    }
}

impl PanelText for TextEngine {
    /// One shaping call, one glyph walk.
    ///
    /// `Buffer::draw` would do the walk. But it offers no place for a
    /// per-span baseline shift, and no place to read a span back off a
    /// glyph. Therefore the walk lives here: cosmic-text's own two
    /// lines plus the shift, which enters as the glyph's own y offset.
    fn draw_run(&mut self, run: DrawRun<'_>, target: &mut PixmapMut<'_>) {
        // Use the family the config resolved to, not the theme's name.
        // Measurement takes the name the scene carries. Paint takes
        // the name that is installed. The borrows must stay disjoint -
        // `family` here, and `fonts` plus `swash` below - which is why
        // `shape` is an associated function.
        let family = self.family.as_str();
        let spans: Vec<StyledSpan<'_>> =
            run.spans.iter().map(|s| StyledSpan { font: family, ..*s }).collect();
        let buffer = TextEngine::shape(&mut self.fonts, &spans, run.max_w);
        // cosmic-text snaps the glyph raster to the pixel grid, so the
        // wrap box's own origin snaps too. A fractional pen would only
        // smear the hinting.
        let (ox, oy) = (round(run.origin.0), round(run.origin.1));
        let (w, h) = (target.width() as i32, target.height() as i32);
        let stride = target.width() as usize;
        let px = target.pixels_mut();
        let mut bases = LineBases::default();
        for line in buffer.layout_runs() {
            let base = bases.advance(&spans, &line);
            for glyph in line.glyphs {
                // A glyph that no span claims keeps the first span's
                // color and sits on the baseline. The seam's own
                // measurement skips such a glyph too.
                let (index, span) = match span_at(&spans, base + glyph.start) {
                    Some(found) => found,
                    None => (0, &spans[0]),
                };
                let shift = run.shifts.get(index).copied().unwrap_or(0.0);
                // `verticalAlign` raises the glyph off the baseline
                // that its line reported. The measured baseline exists
                // for this arithmetic.
                let placed = glyph.physical((0.0, line.line_y - shift), 1.0);
                let (r, g, b) = span.color;
                let (gx, gy) = (placed.x.saturating_add(ox), placed.y.saturating_add(oy));
                self.swash.with_pixels(
                    &mut self.fonts,
                    placed.cache_key,
                    Color::rgb(r, g, b),
                    |dx, dy, color| {
                        // Straight alpha: for a mask glyph,
                        // cosmic-text hands back the base RGB with the
                        // coverage in alpha. Every face this popup
                        // loads makes mask glyphs. Color bitmaps
                        // (emoji) come through the same arm and get
                        // premultiplied a second time. That costs a
                        // shade of saturation, on a path a dictionary
                        // popup rarely takes.
                        let a = u32::from(color.a());
                        if a == 0 {
                            return;
                        }
                        // Clip, and do not trust the numbers.
                        // cosmic-text reports the glyph's ink box.
                        // That box overhangs the wrap box on both
                        // sides, and it goes negative for a leading
                        // side bearing.
                        let (x, y) = (gx.saturating_add(dx), gy.saturating_add(dy));
                        if x < 0 || y < 0 || x >= w || y >= h {
                            return;
                        }
                        let (r, g, b) =
                            (u32::from(color.r()), u32::from(color.g()), u32::from(color.b()));
                        let inv = 255 - a;
                        let i = y as usize * stride + x as usize;
                        let dst = px[i];
                        // Premultiplied source-over onto the panel
                        // background, which the pixmap already holds.
                        // The result keeps `rgb <= a`, because each
                        // channel is monotone in a value that already
                        // kept that bound. `from_rgba` therefore
                        // cannot refuse the result.
                        if let Some(c) = PremultipliedColorU8::from_rgba(
                            over(r, a, u32::from(dst.red()), inv),
                            over(g, a, u32::from(dst.green()), inv),
                            over(b, a, u32::from(dst.blue()), inv),
                            (a + div255(u32::from(dst.alpha()) * inv)) as u8,
                        ) {
                            px[i] = c;
                        }
                    },
                );
            }
        }
    }
}

/// `v / 255`, rounded, without a divide.
#[inline]
fn div255(v: u32) -> u32 {
    let v = v + 128;
    (v + (v >> 8)) >> 8
}

/// One channel of premultiplied source-over.
#[inline]
fn over(src: u32, a: u32, dst: u32, inv: u32) -> u8 {
    (div255(src * a) + div255(dst * inv)) as u8
}

#[inline]
fn round(v: f32) -> i32 {
    v.round() as i32
}

/// Where the current buffer line starts, over a run's whole text.
///
/// Glyph offsets are relative to their *buffer line*. `set_rich_text`
/// splits the spans' text on line endings and strips those endings.
/// Any code that maps a glyph back to its source text must therefore
/// accumulate the lines' bases. Core's runs are single lines today.
/// This struct keeps the answer right when one run is not.
#[derive(Default)]
struct LineBases {
    /// Bytes before the current line.
    base: usize,
    /// The buffer line that `base` counts to.
    line: Option<usize>,
    /// That line's length in bytes.
    len: usize,
}

impl LineBases {
    /// The base for `line`, which must not precede the last line asked
    /// for. Layout runs arrive top down.
    fn advance(&mut self, spans: &[StyledSpan<'_>], line: &LayoutRun<'_>) -> usize {
        if self.line != Some(line.line_i) {
            if self.line.is_some() {
                self.base += self.len + ending_len(spans, self.base + self.len);
            }
            self.line = Some(line.line_i);
            self.len = line.text.len();
        }
        self.base
    }
}

/// The byte offset `utf16` names in a run's spans, end to end.
///
/// DirectWrite hit-tests UTF-16 code-unit offsets natively.
/// cosmic-text is UTF-8, and its glyph clusters are byte ranges, so
/// the conversion lands here. A walk is the honest method: once the
/// text leaves the BMP, a UTF-16 offset has no arithmetic relation to
/// a byte offset. Japanese text reaches past the BMP, with astral
/// kanji and emoji in a gloss. An offset past the end answers the end.
fn byte_offset(spans: &[StyledSpan<'_>], utf16: u32) -> usize {
    let mut units = 0u32;
    let mut base = 0usize;
    for span in spans {
        for (byte, ch) in span.text.char_indices() {
            if units >= utf16 {
                return base + byte;
            }
            units += ch.len_utf16() as u32;
        }
        base += span.text.len();
    }
    base
}

/// The UTF-16 offset at a byte boundary in a run's concatenated spans.
///
/// cosmic-text reports a byte index within one buffer line. Hit testing first
/// adds that line's base, then this walk restores the seam's UTF-16 address.
fn utf16_offset(spans: &[StyledSpan<'_>], byte: usize) -> u32 {
    let mut units = 0u32;
    let mut base = 0usize;
    for span in spans {
        for (offset, ch) in span.text.char_indices() {
            if base + offset >= byte {
                return units;
            }
            units += ch.len_utf16() as u32;
        }
        base += span.text.len();
        if byte <= base {
            return units;
        }
    }
    units
}

/// The span covering byte offset `at`, and its index.
///
/// The search is linear. A paragraph carries a handful of spans, and a
/// search structure would cost more to build than it saves.
fn span_at<'a, 's>(
    spans: &'a [StyledSpan<'s>],
    at: usize,
) -> Option<(usize, &'a StyledSpan<'s>)> {
    let mut end = 0usize;
    for (i, span) in spans.iter().enumerate() {
        end += span.text.len();
        if at < end {
            return Some((i, span));
        }
    }
    None
}

/// The box of the cluster covering UTF-16 offset `utf16`.
///
/// A line-ending offset matches no glyph. It answers the zero-width end of
/// the line before it. An offset past the text or inside an unexpected cluster
/// answers the end of the last line. This function never skips an offset.
fn caret_box(buffer: &Buffer, spans: &[StyledSpan<'_>], utf16: u32) -> GlyphBox {
    let target = byte_offset(spans, utf16);
    // A run with no lines at all has no shaped height to report, so
    // the first span's own advance answers instead.
    let empty = line_height(spans.first().map_or(0.0, |s| s.size));
    let mut end = GlyphBox { x: 0.0, y: 0.0, w: 0.0, h: empty };
    let mut bases = LineBases::default();
    for run in buffer.layout_runs() {
        let base = bases.advance(spans, &run);
        // cosmic-text strips a hard line ending. When the next line advances
        // the base past `target`, the prior visual run owns that caret.
        if target < base {
            return end;
        }
        // The caret is as tall as the line it lands on. A small span
        // beside a large one therefore still gives a full-height hit
        // target.
        let h = run.line_height;
        end = GlyphBox { x: run.line_w, y: run.line_top, w: 0.0, h };
        for glyph in run.glyphs {
            if (base + glyph.start..base + glyph.end).contains(&target) {
                return GlyphBox { x: glyph.x, y: run.line_top, w: glyph.w, h };
            }
        }
    }
    end
}

/// The byte at `at`, over a run's spans end to end.
fn byte_at(spans: &[StyledSpan<'_>], at: usize) -> Option<u8> {
    let mut base = 0usize;
    for span in spans {
        let bytes = span.text.as_bytes();
        if let Some(b) = at.checked_sub(base).and_then(|i| bytes.get(i)) {
            return Some(*b);
        }
        base += bytes.len();
    }
    None
}

/// The line ending `set_rich_text` stripped at `at`.
fn ending_len(spans: &[StyledSpan<'_>], at: usize) -> usize {
    match byte_at(spans, at) {
        Some(b'\r') if byte_at(spans, at + 1) == Some(b'\n') => 2,
        Some(b'\r' | b'\n') => 1,
        _ => 0,
    }
}

/// True when `family` renders `text` as something other than tofu.
fn covers(fonts: &mut FontSystem, family: &str, text: &str) -> bool {
    // Wide enough that nothing wraps. The probe cares only about glyph
    // ids, and a wrap would not change them anyway.
    let buffer = TextEngine::shape(fonts, &[probe_span(text, family, 16.0)], 1024.0);
    let mut any = false;
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            // Glyph 0 is `.notdef`. The user sees that box when no
            // face in the fallback chain covered the character.
            if glyph.glyph_id == 0 {
                return false;
            }
            any = true;
        }
    }
    any
}

// ---- the Japanese-font probe ----

/// What kanji look like on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JpFonts {
    /// A Japanese-capable family is installed and kanji resolve.
    Present { family: String },
    /// Kanji resolve, but only from a Chinese or Korean family. Han
    /// unification therefore gives the wrong glyph shapes for
    /// Japanese.
    WrongVariant { family: String },
    /// No CJK coverage at all: kanji are tofu.
    Missing,
}

/// Family-name markers that mean "this family draws Japanese".
///
/// The entries are substrings, folded to lower case. They carry two
/// shapes of evidence. Evidence one is an explicit JP locale tag, such
/// as `CJK JP` or ` JP`. Evidence two is the name of a family that
/// Linux distributions ship Japanese in. The bare `gothic` and
/// `mincho` entries name the Japanese type classes, and they catch the
/// long tail: `TakaoGothic`, `Sazanami Mincho`, and distro-renamed IPA
/// builds. [`NON_JP`] and [`FALSE_GOTHIC`] veto those two entries
/// first.
const JP_MARKERS: &[&str] = &[
    "cjk jp",
    " jp",
    "hiragino",
    "meiryo",
    "yu gothic",
    "yu mincho",
    "ipaexgothic",
    "ipaexmincho",
    "ipagothic",
    "ipamincho",
    "ipapgothic",
    "vl gothic",
    "vl pgothic",
    "takao",
    "migu",
    "mplus",
    "m+",
    "sazanami",
    "kochi",
    "koruri",
    "gothic",
    "mincho",
];

/// CJK families that are not Japanese.
///
/// This table runs before [`JP_MARKERS`], so a name that matches both
/// tables loses. That order keeps `Noto Sans CJK SC` out of the JP
/// bucket, because the name holds no JP tag but does hold `Sans`. The
/// same order keeps out `Hiragino Sans GB`, a Simplified Chinese face
/// with a Japanese vendor's name, and `Gothic A1`, which is Korean. A
/// `WrongVariant` verdict also points at a name from this table.
const NON_JP: &[&str] = &[
    "cjk sc",
    "cjk tc",
    "cjk kr",
    "cjk hk",
    "cjk cn",
    "cjk tw",
    "sans sc",
    "sans tc",
    "sans kr",
    "sans hk",
    "sans cn",
    "sans tw",
    "serif sc",
    "serif tc",
    "serif kr",
    "serif hk",
    "serif cn",
    "serif tw",
    "wenquanyi",
    "wqy",
    "droid sans fallback",
    "ar pl",
    "microsoft yahei",
    "simsun",
    "simhei",
    "nsimsun",
    "songti",
    "heiti",
    "pingfang",
    "hiragino sans gb",
    "nanum",
    "malgun",
    "batang",
    "gulim",
    "dotum",
    "gothic a1",
    "spoqa han sans",
];

/// Faces whose name carries "Gothic" for a reason unrelated to
/// Japanese, and which cover no kanji at all.
///
/// Two groups land here. Group one is the American type term, such as
/// `Century Gothic`. Group two is `Noto Sans Gothic`, which draws
/// Wulfila's Gothic *script*: `U+10330..U+1034A` plus a handful of
/// combining marks, and nothing else. That face ships in the same
/// `noto-fonts` package every desktop already has. Without this table,
/// one such face installed beside a Chinese CJK font would pass as
/// Japanese and would suppress the warning the user needs. The
/// settings font combo ([`jp_capable`]) would then offer a family that
/// can draw only tofu.
const FALSE_GOTHIC: &[&str] = &[
    "century gothic",
    "urw gothic",
    "franklin gothic",
    "news gothic",
    "trade gothic",
    "copperplate gothic",
    "letter gothic",
    "highway gothic",
    "alternate gothic",
    "noto sans gothic",
];

/// Trailing locale tags that mean "this CJK family is not Japanese".
///
/// The match runs against the last space-separated token, because only
/// that position is unambiguous. `Sarasa Gothic SC` is Simplified
/// Chinese, and `Sarasa Gothic J` is Japanese. [`NON_JP`]'s infix
/// entries (`cjk sc`, `sans sc`) miss both names. A Latin small-caps
/// face such as `Alegreya SC` trips this table too, and that costs
/// nothing: the name never matched [`JP_MARKERS`], and [`classify`]
/// prefers a recognized CJK name when it must point at one.
const LOCALE_TAGS: &[&str] = &["sc", "tc", "hc", "cn", "tw", "hk", "kr", "k"];

fn locale_tagged(lower: &str) -> bool {
    lower.rsplit(' ').next().is_some_and(|tag| LOCALE_TAGS.contains(&tag))
}

/// Which of [`classify`]'s buckets holds a family name.
///
/// `bucket` lowercases once, passes over the tables once, and records
/// the veto order in one place. [`classify`] needs all four answers,
/// and [`jp_capable`] needs one of them. A second copy of that order
/// would be a second definition of "Japanese font".
enum Bucket {
    /// A name the tables read as Japanese.
    Jp,
    /// A [`NON_JP`] name: recognized, and clearly not Japanese.
    NonJp,
    /// A [`LOCALE_TAGS`] name: not Japanese, and not reliably CJK either.
    Tagged,
    /// A name no table claims - every ordinary Latin face.
    Neutral,
}

fn bucket(family: &str) -> Bucket {
    let lower = family.to_ascii_lowercase();
    if matches_any(&lower, NON_JP) {
        Bucket::NonJp
    } else if locale_tagged(&lower) {
        Bucket::Tagged
    } else if !matches_any(&lower, FALSE_GOTHIC) && matches_any(&lower, JP_MARKERS) {
        Bucket::Jp
    } else {
        Bucket::Neutral
    }
}

/// True when this family name reads as Japanese.
///
/// This is the per-name half of [`classify`], for the settings font
/// combo (ARCHITECTURE.md#settings-and-config: fontdb's JP-capable
/// families fill the combo). The check asks about names only. It runs
/// no shaping, so it cannot know what a face's cmap holds, and
/// [`classify`] can. That trade suits a combo, which offers candidates
/// and passes no verdict on the machine.
pub fn jp_capable(family: &str) -> bool {
    matches!(bucket(family), Bucket::Jp)
}

/// The verdict for a set of installed families.
///
/// The function is pure, so a test can exercise the tables above with
/// no font stack. `kanji_covered` comes from real kanji shaping
/// ([`TextEngine::probe`]) and outranks every name. A name table
/// cannot know what a face's cmap holds. Therefore no coverage means
/// [`JpFonts::Missing`], even when the installed names look Japanese.
pub fn classify(families: &[&str], kanji_covered: bool) -> JpFonts {
    if !kanji_covered {
        return JpFonts::Missing;
    }
    // Two buckets of "not Japanese", because the warning can say more
    // about one than the other. The tables recognize a `NON_JP`
    // family, so the warning can name it. A merely locale-tagged
    // family is no proof of CJK at all, so the warning must not name
    // it.
    let mut named = None;
    let mut tagged = None;
    for name in families {
        match bucket(name) {
            Bucket::Jp => return JpFonts::Present { family: (*name).to_string() },
            Bucket::NonJp => named = named.or(Some(*name)),
            Bucket::Tagged => tagged = tagged.or(Some(*name)),
            Bucket::Neutral => {}
        }
    }
    // Kanji resolved, so *something* covers them, even when no name in
    // the tables claims them. A bundled pan-CJK face is one example.
    // Report that fact, and name no family the tables did not
    // recognize.
    JpFonts::WrongVariant {
        family: named
            .or(tagged)
            .map_or_else(|| "a non-Japanese CJK family".to_string(), str::to_string),
    }
}

fn matches_any(lower: &str, table: &[&str]) -> bool {
    table.iter().any(|marker| lower.contains(marker))
}

/// The one-line diagnostic for a bad verdict.
///
/// The text reaches the daemon log and the tray, so it matches the
/// house style of the other degrade-visibly messages (`cursor:`,
/// `config:`). That style is one line, lower case, and no trailing
/// period. The text also names the package, and leaves the user no
/// guess.
pub fn warning(verdict: &JpFonts) -> Option<String> {
    match verdict {
        JpFonts::Present { .. } => None,
        JpFonts::WrongVariant { family } => Some(format!(
            "font: no Japanese family found - kanji will render with Chinese or Korean glyph \
             variants from {family}; install the {PACKAGE} package (Noto Sans CJK JP)"
        )),
        JpFonts::Missing => Some(format!(
            "font: no Japanese family found - kanji will render as tofu; install the {PACKAGE} \
             package (Noto Sans CJK JP)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The family the Linux theme default names.
    const JP: &str = "Noto Sans CJK JP";

    /// A test that needs real glyph geometry needs a real Japanese
    /// face, and a CI runner carries almost no fonts. Such a test
    /// prints a skip line instead of a failure.
    fn jp_engine() -> Option<TextEngine> {
        let mut engine = TextEngine::new(JP);
        match engine.probe() {
            JpFonts::Present { .. } => Some(engine),
            verdict => {
                eprintln!("skipping: {}", warning(&verdict).unwrap_or_default());
                None
            }
        }
    }

    /// One themed span, as the scene names it.
    fn span(text: &str, size: f32) -> StyledSpan<'_> {
        StyledSpan { text, font: JP, size, weight: 400, italic: false, color: (0, 0, 0) }
    }

    /// The same span, in `weight`.
    fn heavy(text: &str, size: f32, weight: u16) -> StyledSpan<'_> {
        StyledSpan { weight, ..span(text, size) }
    }

    /// One run measured through the seam.
    fn measured(engine: &mut TextEngine, spans: &[StyledSpan<'_>], max_w: f32) -> Measured {
        let mut out = Measured::default();
        engine
            .measure(MeasureRun { spans, max_w }, &mut out)
            .expect("cosmic-text never refuses a run");
        out
    }

    /// The face a shaped run's glyphs came from, as fontdb names it.
    fn face_of_shape(
        engine: &mut TextEngine,
        spans: &[StyledSpan<'_>],
        max_w: f32,
    ) -> Option<String> {
        let buffer = TextEngine::shape(&mut engine.fonts, spans, max_w);
        let mut glyphs = buffer.layout_runs().flat_map(|line| line.glyphs.iter());
        let id = glyphs.next()?.font_id;
        engine.fonts.db().face(id).map(|face| face.post_script_name.clone())
    }

    fn face_of(engine: &mut TextEngine, text: &str, family: &str) -> Option<String> {
        face_of_shape(engine, &[probe_span(text, family, 20.0)], 400.0)
    }

    /// True when `family` ships a bold face.
    fn ships_bold(engine: &TextEngine, family: &str) -> bool {
        engine.fonts.db().faces().any(|face| {
            face.weight == Weight::BOLD
                && face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    /// A themed weight reaches the shaper.
    ///
    /// This test states what core's per-role weights buy on Linux. A
    /// bold role must resolve to the family's bold face. Before CSS
    /// theming, every run shaped regular and reported nothing. The
    /// test skips when the family ships one weight, because fontdb
    /// then answers with the nearest face it has, and the two results
    /// match.
    #[test]
    fn a_bold_run_shapes_in_the_familys_bold_face() {
        let Some(mut engine) = jp_engine() else { return };
        if !ships_bold(&engine, JP) {
            eprintln!("skipping: {JP} ships no bold face");
            return;
        }
        let regular = face_of_shape(&mut engine, &[span(PROBE_TEXT, 20.0)], 400.0);
        let bold = face_of_shape(&mut engine, &[heavy(PROBE_TEXT, 20.0, 700)], 400.0);
        assert!(regular.is_some(), "the probe text must shape");
        assert_ne!(regular, bold, "a bold role must not shape in the regular face");
    }

    /// The one invariant this whole module protects. With no Japanese
    /// family named, kanji still resolve through cosmic-text's Han
    /// unification. At locale `ja`, that path lands on the JP face,
    /// and not on the Simplified Chinese one. A regression here draws
    /// Chinese glyph variants for every kanji the popup shows, and
    /// nothing else fails. Therefore this test asserts the invariant
    /// and trusts nothing.
    #[test]
    fn kanji_fall_back_to_the_japanese_face_and_never_the_chinese_one() {
        let Some(mut engine) = jp_engine() else { return };
        // A family that exists nowhere: only the script fallback can
        // answer, and the locale decides that path.
        let Some(face) = face_of(&mut engine, "\u{6f22}\u{5b57}", "chibipop-no-such-family") else {
            eprintln!("skipping: the font stack produced no glyphs at all");
            return;
        };
        let lower = face.to_ascii_lowercase();
        assert!(lower.contains("jp"), "kanji fell back to {face}, which is not a Japanese face");
        // Whole tags only. `NotoSansCJKjp-Regular` holds a bare "sc"
        // inside "SansCJK", so this list names the Chinese and Korean
        // faces by the tags they carry.
        for wrong in ["cjksc", "cjktc", "cjkhk", "cjkkr", "cjkkorean"] {
            assert!(!lower.contains(wrong), "kanji fell back to {face}: the wrong Han variants");
        }
    }

    #[test]
    fn a_japanese_family_that_covers_kanji_is_present() {
        assert_eq!(
            JpFonts::Present { family: JP.to_string() },
            classify(&["DejaVu Sans", JP], true)
        );
    }

    #[test]
    fn a_chinese_family_alone_means_the_wrong_glyph_variants() {
        assert_eq!(
            JpFonts::WrongVariant { family: "Noto Sans CJK SC".to_string() },
            classify(&["Noto Sans CJK SC"], true),
            "CJK SC contains no JP tag and must never pass as Japanese"
        );
    }

    #[test]
    fn no_kanji_coverage_at_all_is_missing() {
        assert_eq!(JpFonts::Missing, classify(&[], false));
        assert_eq!(
            JpFonts::Missing,
            classify(&[JP], false),
            "the shaping probe outranks the name table"
        );
    }

    #[test]
    fn japanese_family_detection_ignores_case() {
        assert_eq!(
            JpFonts::Present { family: "noto sans cjk jp".to_string() },
            classify(&["noto sans cjk jp"], true)
        );
        assert_eq!(
            JpFonts::Present { family: "IPAEXGOTHIC".to_string() },
            classify(&["IPAEXGOTHIC"], true)
        );
    }

    #[test]
    fn a_japanese_family_wins_over_an_installed_chinese_one() {
        let both = ["Noto Sans CJK SC", "WenQuanYi Zen Hei", JP];
        assert_eq!(JpFonts::Present { family: JP.to_string() }, classify(&both, true));
    }

    /// `Noto Sans Gothic` is the face that matters in practice. It
    /// draws the Gothic script, covers no kanji, and is already
    /// installed wherever `noto-fonts` is installed.
    #[test]
    fn faces_named_gothic_for_other_reasons_are_not_japanese() {
        for gothic in ["Century Gothic", "Noto Sans Gothic"] {
            assert_eq!(
                JpFonts::WrongVariant { family: "Noto Sans CJK SC".to_string() },
                classify(&[gothic, "Noto Sans CJK SC"], true),
                "{gothic} must not suppress the wrong-variant warning"
            );
            assert!(!jp_capable(gothic), "{gothic} must not reach the settings font combo");
        }
    }

    /// Sarasa ships one family per locale, and the locale tag is the
    /// last token. The type-class markers alone therefore cannot
    /// separate the cuts.
    #[test]
    fn a_trailing_locale_tag_beats_the_gothic_marker() {
        assert_eq!(
            JpFonts::WrongVariant { family: "Sarasa Gothic SC".to_string() },
            classify(&["Sarasa Gothic SC"], true)
        );
        assert_eq!(
            JpFonts::Present { family: "Sarasa Gothic J".to_string() },
            classify(&["Sarasa Gothic J"], true),
            "the Japanese cut of the same family still has to pass"
        );
    }

    #[test]
    fn the_wrong_variant_warning_names_a_recognised_cjk_family_not_a_small_caps_latin_one() {
        assert_eq!(
            JpFonts::WrongVariant { family: "Noto Sans CJK SC".to_string() },
            classify(&["Alegreya SC", "Noto Sans CJK SC"], true)
        );
    }

    #[test]
    fn the_warning_names_the_package_for_both_bad_verdicts_and_stays_quiet_otherwise() {
        assert_eq!(None, warning(&JpFonts::Present { family: JP.to_string() }));
        for verdict in [
            JpFonts::Missing,
            JpFonts::WrongVariant { family: "Noto Sans CJK SC".to_string() },
        ] {
            let line = warning(&verdict).expect("a bad verdict warns");
            assert!(line.contains(PACKAGE), "{line}");
            assert!(line.starts_with("font: "), "{line}");
            assert!(!line.ends_with('.'), "{line}");
            assert_eq!(1, line.lines().count(), "{line}");
        }
    }

    #[test]
    fn a_family_the_font_database_lacks_is_not_resolvable() {
        let engine = TextEngine::new(JP);
        assert!(
            !engine.resolvable("Chibipop Nonexistent Sans"),
            "an honest false is what surfaces FontChoice::Fallback"
        );
    }

    #[test]
    fn the_probe_names_the_family_the_popup_will_paint_with() {
        let mut engine = TextEngine::new(JP);
        if !engine.resolvable(JP) {
            eprintln!("skipping: {JP} is not installed");
            return;
        }
        assert_eq!(JpFonts::Present { family: JP.to_string() }, engine.probe());
    }

    /// The safety property this test locks. A wider seam must not move
    /// a number. Every assertion here repeats the arithmetic of the
    /// one-string seam: `lines × size × LINE_HEIGHT` for the height,
    /// and the widest `line_w` for the width. A single-span request
    /// that drifts therefore fails here, and not in a golden.
    #[test]
    fn an_empty_run_measures_no_width_and_exactly_one_line() {
        let Some(mut engine) = jp_engine() else { return };
        let m = measured(&mut engine, &[span("", 16.0)], 200.0);
        assert_eq!(0.0, m.metrics.w);
        assert_eq!(1, m.metrics.lines);
        assert_eq!(16.0 * LINE_HEIGHT, m.metrics.h);
        assert_eq!(1, m.lines.len(), "one line box per counted line");
        assert_eq!(16.0 * LINE_HEIGHT, m.lines[0].h);
        assert!(m.spans.is_empty(), "no glyphs, no span boxes");
    }

    #[test]
    fn a_long_japanese_run_wraps_and_its_height_is_whole_lines() {
        let Some(mut engine) = jp_engine() else { return };
        let text = "\u{8f9e}\u{66f8}\u{306e}\u{8aac}\u{660e}\u{6587}\u{3092}\u{72ed}\u{3044}\
                    \u{5e45}\u{3067}\u{6298}\u{308a}\u{8fd4}\u{3059}\u{305f}\u{3081}\u{306e}\
                    \u{9577}\u{3044}\u{6587}\u{7ae0}";
        let m = measured(&mut engine, &[span(text, 16.0)], 60.0);
        let n = m.metrics.lines;
        assert!(n > 1, "{n} lines at max_w 60");
        assert_eq!(n as f32 * 16.0 * LINE_HEIGHT, m.metrics.h, "runs stack by whole lines");
        assert!(m.metrics.w <= 60.0, "wrapped width {} exceeds the wrap box", m.metrics.w);
        // The detail agrees with the aggregate it sits beside.
        assert_eq!(n as usize, m.lines.len());
        let widest = m.lines.iter().fold(0.0f32, |a, l| a.max(l.w));
        assert_eq!(m.metrics.w, widest, "the aggregate width is the widest line");
        assert_eq!(0.0, m.lines[0].y, "the first line sits at the run's top");
        for pair in m.lines.windows(2) {
            // The shaper reports its own running top, and it paints
            // from that top. The aggregate height sums wide and rounds
            // once instead, so the two can differ by an ulp at eight
            // lines. The block stack still must not move.
            assert_eq!(pair[0].y + pair[0].h, pair[1].y, "a line starts where the last ended");
        }
        for line in &m.lines {
            assert_eq!(16.0 * LINE_HEIGHT, line.h);
            assert!(line.baseline > 0.0 && line.baseline < line.h, "{line:?}");
        }
        // One span, so one box per line. Each box starts at the margin.
        assert_eq!(n as usize, m.spans.len());
        for (i, b) in m.spans.iter().enumerate() {
            assert_eq!((0, i as u32), (b.span, b.line));
            assert_eq!(0.0, b.x);
            assert_eq!(m.lines[i].w, b.w, "the only span on a line fills it");
            assert_eq!(16.0 * LINE_HEIGHT, b.h);
        }
    }

    /// The whole point of the widening: two styles on one line, each
    /// with its own box, and all of them hung from one baseline. This
    /// test asserts against real cosmic-text output, and not against a
    /// fake. The shaper decides a mixed line's height, and core does
    /// not.
    #[test]
    fn spans_of_two_sizes_share_a_line_and_a_baseline() {
        let Some(mut engine) = jp_engine() else { return };
        // 漢 at body size, and 字 at half of it. The box is wide
        // enough that neither span wraps.
        let spans = [span("\u{6f22}", 20.0), span("\u{5b57}", 10.0)];
        let m = measured(&mut engine, &spans, 400.0);
        assert_eq!(1, m.metrics.lines, "both spans fit one line");
        assert_eq!(2, m.spans.len(), "one box per span");

        let (big, small) = (m.spans[0], m.spans[1]);
        assert_eq!((0, 0), (big.span, big.line));
        assert_eq!((1, 0), (small.span, small.line));
        assert_eq!(0.0, big.x, "the first span starts at the margin");
        assert!(big.w > small.w, "a 20px kanji is wider than a 10px one");
        assert_eq!(big.w, small.x, "the second span starts where the first ends");
        assert_eq!(small.x + small.w, m.lines[0].w, "the spans sum to the line");

        // Each span asks for its own advance. The line takes the
        // maximum.
        assert_eq!(line_height(20.0), big.h);
        assert_eq!(line_height(10.0), small.h);
        assert_eq!(line_height(20.0), m.lines[0].h, "the taller span sets the line");
        assert_eq!(m.lines[0].h, m.metrics.h);
        // One baseline for the line, and inside the line. That
        // property is the reason the seam must report a baseline.
        let base = m.lines[0].baseline;
        assert!(base > 0.0 && base < m.lines[0].h, "baseline {base} outside the line");
    }

    /// The walk hands one `Measured` to every element in a panel. A
    /// measurer that appended instead of clearing would grow each
    /// element's geometry by every element before it. The panel would
    /// still produce a layout, and that layout would be wrong.
    #[test]
    fn one_buffer_measures_two_runs_without_carrying_the_first_over() {
        let Some(mut engine) = jp_engine() else { return };
        let long = measured(&mut engine, &[span("\u{6f22}\u{5b57}\u{8f9e}\u{66f8}", 16.0)], 400.0);

        let mut scratch = long.clone();
        engine
            .measure(MeasureRun { spans: &[span(PROBE_TEXT, 16.0)], max_w: 400.0 }, &mut scratch)
            .expect("shapeable");
        let fresh = measured(&mut engine, &[span(PROBE_TEXT, 16.0)], 400.0);
        assert_eq!(fresh, scratch, "a reused buffer answers the run it was just given");
        assert!(scratch.metrics.w < long.metrics.w, "two kanji are narrower than four");
    }

    /// The one place the real shaper meets the real walk. Every other
    /// test drives `layout::scene` from a fake, and drives this module
    /// from core's types. Therefore no other test would see the two
    /// answers differ.
    #[test]
    fn the_real_engine_lays_out_a_whole_panel() {
        let Some(mut engine) = jp_engine() else { return };
        let theme = crate::popup::physical_theme(&chibipop::ui::theme::Theme::dark(), 1.0);
        let p = crate::popup::canned();
        let scene = chibipop::ui::layout::scene(
            &chibipop::ui::layout::SceneRequest {
                presentation: &p,
                theme: &theme,
                max_w: 424.0,
                max_h: 4000.0,
                show_back: true,
                side_panel: true,
                render: Default::default(),
                anki: None,
                selection: None,
            },
            &mut engine,
        )
        .expect("cosmic-text never refuses a run");

        assert!(!scene.elems.is_empty(), "the canned card has content");
        let mut bottom = 0.0f32;
        for elem in &scene.elems {
            assert!(elem.rect.y >= 0.0, "{elem:?} sits above the panel");
            assert!(elem.rect.y >= bottom - f32::EPSILON, "elements go backwards at {elem:?}");
            assert!(elem.rect.w <= scene.content_w + 1.0, "{elem:?} overflows the column");
            bottom = elem.rect.y;
        }
        assert!(scene.used_h > 0.0);
        assert!(scene.content_h >= scene.used_h, "the panel holds what the walk stacked");
    }

    #[test]
    fn a_zero_wrap_width_clamps_instead_of_panicking() {
        let Some(mut engine) = jp_engine() else { return };
        let m = measured(&mut engine, &[span(PROBE_TEXT, 16.0)], 0.0);
        assert!(m.metrics.lines >= 1);
        assert!(m.metrics.h > 0.0);
    }

    #[test]
    fn caret_boxes_answer_one_ordered_box_per_offset() {
        let Some(mut engine) = jp_engine() else { return };
        // 漢字辞書 - four BMP kanji, so one UTF-16 unit each.
        let text = "\u{6f22}\u{5b57}\u{8f9e}\u{66f8}";
        let mut out = Vec::new();
        engine
            .caret_boxes(
                MeasureRun { spans: &[span(text, 20.0)], max_w: 400.0 },
                &[0, 1, 2, 3],
                &mut out,
            )
            .expect("shapeable");
        assert_eq!(4, out.len());
        for pair in out.windows(2) {
            assert!(pair[0].x <= pair[1].x, "{out:?}");
        }
        for glyph in &out {
            assert!(glyph.w > 0.0, "a kanji has width: {glyph:?}");
            assert_eq!(20.0 * LINE_HEIGHT, glyph.h);
        }
    }

    /// Drill-down probes a headword that can arrive as several styled
    /// spans. An offset must therefore count over the run's whole
    /// text, and not over the first span alone.
    #[test]
    fn caret_offsets_run_on_across_a_span_boundary() {
        let Some(mut engine) = jp_engine() else { return };
        let spans = [span("\u{6f22}", 20.0), span("\u{5b57}", 20.0)];
        let mut out = Vec::new();
        engine
            .caret_boxes(MeasureRun { spans: &spans, max_w: 400.0 }, &[0, 1], &mut out)
            .expect("shapeable");
        assert_eq!(2, out.len());
        assert_eq!(0.0, out[0].x, "the first kanji sits at the margin");
        assert!(out[1].x >= out[0].w, "the second kanji is in the second span: {out:?}");
        assert!(out[1].w > 0.0, "an offset in a later span still finds a glyph");
    }

    #[test]
    fn caret_at_a_hard_line_end_stays_on_that_line() {
        let Some(mut engine) = jp_engine() else { return };
        let text = "① first sense\n② second sense";
        let spans = [span(text, 20.0)];
        let run = MeasureRun { spans: &spans, max_w: 400.0 };
        let measured = measured(&mut engine, &spans, run.max_w);
        let newline = text.find('\n').expect("hard line end");
        let offset = text[..newline].encode_utf16().count() as u32;
        let mut out = Vec::new();
        engine.caret_boxes(run, &[offset], &mut out).expect("shapeable");

        assert_eq!(0.0, out[0].w);
        assert_eq!(measured.lines[0].y, out[0].y);
        assert_eq!(measured.lines[0].w, out[0].x);
    }


    /// Hit testing converts cosmic-text byte positions back to UTF-16 offsets.
    #[test]
    fn hit_offset_maps_astral_text_and_clamps_vertical_points() {
        let Some(mut engine) = jp_engine() else { return };
        let text = "\u{6f22}\u{1f363}\u{5bff}";
        let spans = [span(text, 20.0)];
        let run = MeasureRun { spans: &spans, max_w: 400.0 };
        let mut boxes = Vec::new();
        engine.caret_boxes(run, &[0, 1, 3], &mut boxes).expect("shapeable");
        let y = boxes[0].y + boxes[0].h / 2.0;

        assert_eq!(0, engine.hit_offset(run, boxes[0].x + 0.1, y).unwrap());
        assert_eq!(1, engine.hit_offset(run, boxes[1].x + 0.1, y).unwrap());
        assert_eq!(3, engine.hit_offset(run, boxes[2].x + 0.1, y).unwrap());
        assert_eq!(0, engine.hit_offset(run, 0.0, -1.0).unwrap());
        assert_eq!(4, engine.hit_offset(run, 0.0, 1000.0).unwrap());
    }
    #[test]
    fn an_offset_past_the_text_answers_a_zero_width_box_rather_than_nothing() {
        let Some(mut engine) = jp_engine() else { return };
        let mut out = Vec::new();
        engine
            .caret_boxes(
                MeasureRun { spans: &[span(PROBE_TEXT, 20.0)], max_w: 400.0 },
                &[0, 99],
                &mut out,
            )
            .expect("shapeable");
        assert_eq!(2, out.len(), "core zips these 1:1 and cannot take a gap");
        assert_eq!(0.0, out[1].w);
    }

    #[test]
    fn utf16_offsets_step_over_a_surrogate_pair() {
        let Some(mut engine) = jp_engine() else { return };
        // 🍣 is astral: two UTF-16 units and four bytes. 寿 therefore
        // starts at UTF-16 offset 2 and byte offset 4. An offset used
        // as an index would land inside the emoji.
        let text = "\u{1f363}\u{5bff}";
        assert_eq!(4, byte_offset(&[span(text, 20.0)], 2));
        let mut out = Vec::new();
        engine
            .caret_boxes(MeasureRun { spans: &[span(text, 20.0)], max_w: 400.0 }, &[2], &mut out)
            .expect("shapeable");
        assert_eq!(1, out.len());
        assert!(out[0].x > 0.0, "寿 sits after the emoji: {:?}", out[0]);
        assert!(out[0].w > 0.0);
    }

    #[test]
    fn drawing_a_kanji_marks_the_pixmap_and_never_writes_past_it() {
        let Some(mut engine) = jp_engine() else { return };
        const W: usize = 48;
        const H: usize = 32;
        // One guard row past the pixmap. A stride overrun writes into
        // that row instead of tripping a slice bound, so this test
        // checks the row directly.
        let mut data = vec![0u8; W * (H + 1) * 4];
        let (front, guard) = data.split_at_mut(W * H * 4);
        for px in front.chunks_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 255]);
        }
        let before = front.to_vec();
        let mut target = PixmapMut::from_bytes(front, W as u32, H as u32).expect("pixmap");

        let draw = |target: &mut PixmapMut<'_>, engine: &mut TextEngine, origin| {
            // White on the black fill below, so the ink is visible.
            let spans = [StyledSpan {
                text: PROBE_TEXT,
                font: "",
                size: 20.0,
                weight: 400,
                italic: false,
                color: (255, 255, 255),
            }];
            engine.draw_run(
                DrawRun { spans: &spans, shifts: &[0.0], max_w: 200.0, origin },
                target,
            );
        };

        draw(&mut target, &mut engine, (2.0, 2.0));
        assert_ne!(before, target.data_mut().to_vec(), "a kanji leaves ink");

        // The edges. A glyph that hangs past the bottom-right corner
        // must clip, and a glyph at a negative origin must clip too.
        // Neither glyph indexes out of the buffer, and neither spills
        // into the next row.
        draw(&mut target, &mut engine, (W as f32 - 3.0, H as f32 - 3.0));
        draw(&mut target, &mut engine, (-8.0, -8.0));
        draw(&mut target, &mut engine, (-4000.0, -4000.0));
        assert!(guard.iter().all(|&b| b == 0), "wrote past the pixmap");
    }

    /// The canned popup with the real engine: a drag range from `text_hit`
    /// must come back as highlight boxes on the next scene. The fixed
    /// metrics of the pointer tests cannot see a shaping mismatch here.
    #[test]
    fn a_drag_over_the_canned_gloss_paints_with_the_real_engine() {
        use chibipop::select::{SelRange, Selections};
        use chibipop::ui::layout::{scene, SceneRequest};
        let Some(mut engine) = jp_engine() else { return };
        let theme = chibipop::ui::theme::Theme { font_name: JP.to_string(), ..chibipop::ui::theme::Theme::dark() };
        let canned = crate::popup::canned();
        fn request<'a>(
            canned: &'a chibipop::present::Presentation,
            theme: &'a chibipop::ui::theme::Theme,
            selection: Option<&'a Selections>,
        ) -> SceneRequest<'a> {
            SceneRequest {
                presentation: canned,
                theme,
                max_w: 424.0,
                max_h: 4000.0,
                show_back: false,
                side_panel: false,
                render: Default::default(),
                anki: None,
                selection,
            }
        }
        let empty = Selections::default();
        let plain = scene(&request(&canned, &theme, Some(&empty)), &mut engine).unwrap();
        let first = plain.elems.iter().find(|e| !e.sources.is_empty()).expect("gloss text");
        let y = first.pen.1 + first.rect.h / 2.0;
        let start = plain.text_hit((first.pen.0 + 1.0, y), 0.0, JP, &mut engine).unwrap().unwrap();
        let end = plain.text_hit((first.pen.0 + 60.0, y), 0.0, JP, &mut engine).unwrap().unwrap();
        assert!(start < end, "{start:?} < {end:?}");
        let mut all = Selections::default();
        all.card_mut(0).replace(SelRange { start, end });
        let selected = scene(&request(&canned, &theme, Some(&all)), &mut engine).unwrap();
        assert!(!selected.highlights.is_empty(), "{start:?}..{end:?} on {:?}", first.text);
        let box_ = selected.highlights[0];
        assert!(box_.w > 0.0 && box_.h > 0.0, "{box_:?}");
    }
}
