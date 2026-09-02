//! The Linux text stack: cosmic-text behind core's `TextMeasure`, plus
//! the paint half and the startup Japanese-font probe.
//!
//! **The locale is the whole feature.** cosmic-text picks a Han fallback
//! through `han_unification(locale)`, and on unix only `"ja"` yields
//! `Noto Sans CJK JP`; every other arm - including the `en-US` that
//! `sys_locale` hands `FontSystem::new()` on a stock desktop - yields
//! `Noto Sans CJK SC`, i.e. *Chinese* glyph shapes for kanji. Nothing
//! errors, nothing logs: the popup just quietly teaches the user the
//! wrong character forms, which is the one failure this product cannot
//! ship. So the `FontSystem` is built by hand with the locale pinned,
//! never by `FontSystem::new()`.
//!
//! **One shaping path.** `measure` and `draw_run` both go through
//! [`shape`], so a run is never wrapped one way and painted another.
//! Windows earns the same property by routing both through one
//! `IDWriteTextLayout`; the twin here is
//! `chibipop-windows/src/ui/render.rs`'s `Text::layout`.
//!
//! **Physical pixels only.** `size` and `max_w` arrive already scaled by
//! `popup::physical_theme`; there is no logical-pixel arithmetic here.
//!
//! Where the twins diverge: DirectWrite hit-tests UTF-16 natively, so
//! Windows passes core's caret offsets straight to
//! `HitTestTextPosition`. cosmic-text is UTF-8 throughout -
//! `LayoutGlyph::start`/`end` are byte ranges into the line - so the
//! UTF-16 offsets core zips against kanji have to be walked back into
//! byte offsets here ([`byte_offset`]).

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
/// Deliberate, and the one number core's line stacking rests on: every
/// run's height is a whole number of these, so blocks stack exactly.
/// Noto Sans CJK JP's `hhea` ascent+descent is 1.448 em with a zero line
/// gap - correct but airy - while Yu Gothic UI through DirectWrite, the
/// density the Windows popup ships and the one users compare against,
/// lands near 1.36 em. 1.4 sits between them: tight enough that a Linux
/// popup is not visibly looser than the Windows one, loose enough that
/// full-height kana and kanji plus a descender never collide, since the
/// panel is painted without clipping and neighbouring lines would
/// otherwise touch ink.
const LINE_HEIGHT: f32 = 1.4;

/// cosmic-text asserts on a zero font size or line height, and a scene
/// that asks for one is a caller bug, not a reason to abort the daemon
/// mid-paint. Clamping also disarms NaN: `f32::max` returns the finite
/// operand.
const MIN_SIZE: f32 = 1.0;

/// The kanji the startup probe shapes: 漢字, two of the commonest
/// ideographs. If these are tofu, everything the popup shows is tofu.
const PROBE_TEXT: &str = "\u{6f22}\u{5b57}";

/// The package to name in the missing-font warning.
///
/// Arch/`noto-fonts-cjk`; Debian and Fedora both ship an alias or a
/// virtual package under this name, and naming one concrete family in
/// the message covers the rest.
pub const PACKAGE: &str = "noto-fonts-cjk";

/// cosmic-text, with the locale pinned and its caches warm.
///
/// One per daemon: building it parses every installed face, which costs
/// the better part of a second.
pub struct TextEngine {
    fonts: FontSystem,
    /// Rasterized glyph images, keyed by face+size+subpixel offset. The
    /// popup re-shapes on every paint and this is what makes that free
    /// after the first frame.
    swash: SwashCache,
    /// The family the config resolved to, used for painting. `measure`
    /// takes its family from the run instead, because core carries the
    /// theme's name through the scene.
    family: String,
}

impl TextEngine {
    pub fn new(family: &str) -> TextEngine {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // The generic aliases (`Family::Serif` and friends) are left at
        // fontdb's defaults on purpose: every run this engine shapes
        // names a concrete family, and the Han fallback list is concrete
        // family names too, so nothing ever consults them.
        //
        // Locale `"ja"`, and never `FontSystem::new()`: see the module
        // doc. The default arm of cosmic-text's `han_unification` is
        // Simplified Chinese, so a system locale of `en-US` would
        // silently render kanji with Chinese glyph variants.
        TextEngine {
            fonts: FontSystem::new_with_locale_and_db("ja".to_string(), db),
            swash: SwashCache::new(),
            family: family.to_string(),
        }
    }

    /// The family paints go through.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Point paints at another family, without reloading the font db.
    ///
    /// Resolution happens *after* the engine exists - `resolve_font`
    /// asks it whether the configured family is installed - and a
    /// reload can change the family again later. Parsing every
    /// installed face a second time for a string swap would cost the
    /// better part of a second on the daemon thread.
    pub fn set_family(&mut self, family: &str) {
        self.family.clear();
        self.family.push_str(family);
    }

    /// Does the font stack have `family`?
    ///
    /// The `resolvable` closure `chibipop::config::resolve_font` wants,
    /// and it has to answer honestly: a `false` is what turns the
    /// configured family into a visible `FontChoice::Fallback`. Folding
    /// is ASCII-only, so localized Japanese family names compare
    /// exactly - which is what the user typed either way.
    pub fn resolvable(&self, family: &str) -> bool {
        self.fonts
            .db()
            .faces()
            .flat_map(|face| face.families.iter())
            .any(|(name, _)| name.eq_ignore_ascii_case(family))
    }

    /// What Japanese will look like on this machine.
    ///
    /// Two independent facts: whether kanji resolve to real glyphs at
    /// all (shaped here, the only authority on tofu) and whether the
    /// family that answers is Japanese (a name question, [`classify`]).
    pub fn probe(&mut self) -> JpFonts {
        // Shape first: `db()` borrows the same `FontSystem` the shaping
        // mutates, and the name list borrows out of the db.
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
            // Ask about the family the popup actually paints with first,
            // so a verdict names that one rather than whichever installed
            // Japanese family happens to sort earliest.
            names.insert(0, &self.family);
        }
        classify(&names, covered)
    }

    /// The one shaping call.
    ///
    /// Measure and paint both come through here, on the same styled
    /// spans, so a run is never wrapped one way and painted another.
    ///
    /// An associated function, not a method, so a caller can hand it
    /// `&mut self.fonts` while still holding `&self.family`.
    fn shape(fonts: &mut FontSystem, spans: &[StyledSpan<'_>], max_w: f32) -> Buffer {
        // The buffer's own metrics answer for a line with no glyphs on
        // it, which is the only line the spans cannot speak for.
        let size = spans.first().map_or(MIN_SIZE, |s| s.size).max(MIN_SIZE);
        let mut buffer = Buffer::new_empty(CosmicMetrics::new(size, size * LINE_HEIGHT));
        // No height bound: shape every wrapped line, not just the ones
        // that would fit a viewport. Core culls off-panel runs itself
        // and needs the full metrics to decide.
        //
        // The width clamp is the same one DirectWrite gets on Windows: a
        // measurer that cannot wrap at zero clamps itself, and the scene
        // still reports the width it asked for.
        buffer.set_size(Some(max_w.max(1.0)), None);
        // Words first, glyphs when a "word" cannot fit alone. Japanese
        // has no spaces, so the segmenter treats runs of ideographs as
        // their own words and this behaves like CJK line breaking; the
        // glyph fallback is what keeps a long Latin headword inside a
        // narrow panel.
        buffer.set_wrap(Wrap::WordOrGlyph);
        // `set_rich_text`, not `set_text`: the spans wrap as one
        // paragraph, so a span boundary is not a line boundary and bold
        // text can share a line with normal text
        // (ARCHITECTURE.md#popup-and-measurement). Per-span `metrics`
        // is what carries each span's own size into the line
        // height cosmic-text takes the maximum of - the default attrs
        // deliberately carry none, so a glyphless line still falls back
        // to the buffer's.
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
/// Weight and style are the theme's, per role (CSS theming): fontdb
/// weights are the same 100-900 numbers DirectWrite takes, so the
/// scene's number travels unconverted. A family with no bold or italic
/// face still shapes - fontdb picks the nearest weight it has, and
/// cosmic-text does not synthesize - so a missing face costs the
/// emphasis, never the run.
fn attrs_of<'a>(span: &StyledSpan<'a>) -> Attrs<'a> {
    Attrs::new()
        .family(Family::Name(span.font))
        .weight(Weight(span.weight))
        .style(if span.italic { Style::Italic } else { Style::Normal })
}

/// One coverage probe's span.
///
/// Regular and upright: a probe asks which face answers for a
/// character, and a family's bold or italic face - if it has one -
/// covers what its regular one does. Colour is not a measurement
/// input, so black is as good as any.
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

/// The line advance a run of `size` stacks by.
fn line_height(size: f32) -> f32 {
    size.max(MIN_SIZE) * LINE_HEIGHT
}

impl TextMeasure for TextEngine {
    /// Never `Err`.
    ///
    /// The fallible signature exists for DirectWrite's HRESULTs; a
    /// pure-Rust shaper has nothing to refuse. Missing glyphs are not a
    /// failure here either - cosmic-text falls back and then emits
    /// `.notdef`, which measures like any other glyph. Tofu is reported
    /// once at startup by [`TextEngine::probe`], not per run.
    fn measure(&mut self, run: MeasureRun<'_>, out: &mut Measured) -> Result<(), MeasureError> {
        out.clear();
        let buffer = TextEngine::shape(&mut self.fonts, run.spans, run.max_w);
        let mut w = 0.0f32;
        // Summed wide and rounded once. Every line of a one-style run
        // is the same advance, and the seam this widened reported
        // `lines × advance` - which repeated `f32` addition drifts an
        // ulp below by the eighth line. A block stack that moves is a
        // golden that moves, so the drift is not affordable.
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
                // `line_y` is the baseline in buffer space; a line box
                // reports it from the line's own top.
                baseline: line.line_y - line.line_top,
            });
            for glyph in line.glyphs {
                let Some((s, span)) = span_at(run.spans, base + glyph.start) else {
                    continue;
                };
                let (s, i) = (s as u32, i as u32);
                // Glyphs arrive in visual order, so a span's box on
                // this line is the last one pushed for it and only
                // ever grows.
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
        // An empty run is one empty line, not zero: core stacks the gap
        // after it either way.
        if out.lines.is_empty() {
            h = f64::from(line_height(run.spans.first().map_or(0.0, |s| s.size)));
            // cosmic-text centres a glyphless line's baseline in it,
            // there being no ascent to hang it from.
            let h = h as f32;
            out.lines.push(LineBox { y: 0.0, w: 0.0, h, baseline: h / 2.0 });
        }
        // Stackable by construction - a whole number of line advances,
        // never an ink box. Core's walk adds these up.
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
        // Exactly one box per offset, in order: core zips these 1:1 with
        // the kanji of a headword to build per-character hit targets, so
        // a skipped offset would silently shift every target after it.
        for &offset in at {
            out.push(caret_box(&buffer, run.spans, offset));
        }
        Ok(())
    }
}

impl PanelText for TextEngine {
    /// One shaping call, one glyph walk.
    ///
    /// `Buffer::draw` would do the walk, but it offers no place to put
    /// a per-span baseline shift and no place to read a span back off
    /// a glyph, so the walk is here: cosmic-text's own two lines plus
    /// the shift, which enters as the glyph's own y offset.
    fn draw_run(&mut self, run: DrawRun<'_>, target: &mut PixmapMut<'_>) {
        // The family the config resolved to, not the theme's name:
        // measuring takes the name the scene carries, painting takes
        // the one that is actually installed. Disjoint field borrows -
        // `family` here, `fonts` and `swash` below - are why `shape` is
        // an associated function.
        let family = self.family.as_str();
        let spans: Vec<StyledSpan<'_>> =
            run.spans.iter().map(|s| StyledSpan { font: family, ..*s }).collect();
        let buffer = TextEngine::shape(&mut self.fonts, &spans, run.max_w);
        // The glyph raster is already snapped to the pixel grid by
        // cosmic-text, so the wrap box's own origin is too; a fractional
        // pen would only smear the hinting.
        let (ox, oy) = (round(run.origin.0), round(run.origin.1));
        let (w, h) = (target.width() as i32, target.height() as i32);
        let stride = target.width() as usize;
        let px = target.pixels_mut();
        let mut bases = LineBases::default();
        for line in buffer.layout_runs() {
            let base = bases.advance(&spans, &line);
            for glyph in line.glyphs {
                // A glyph no span claims - which the seam's own
                // measurement also skips - keeps the first span's
                // colour and sits on the baseline.
                let (index, span) = match span_at(&spans, base + glyph.start) {
                    Some(found) => found,
                    None => (0, &spans[0]),
                };
                let shift = run.shifts.get(index).copied().unwrap_or(0.0);
                // `verticalAlign` raises the glyph off the baseline its
                // line reported, which is the whole arithmetic the
                // measured baseline exists for.
                let placed = glyph.physical((0.0, line.line_y - shift), 1.0);
                let (r, g, b) = span.color;
                let (gx, gy) = (placed.x.saturating_add(ox), placed.y.saturating_add(oy));
                self.swash.with_pixels(
                    &mut self.fonts,
                    placed.cache_key,
                    Color::rgb(r, g, b),
                    |dx, dy, color| {
                        // Straight alpha: for a mask glyph -
                        // everything our faces produce - cosmic-text
                        // hands back the base RGB with the coverage in
                        // alpha. Colour bitmaps (emoji) come through
                        // the same arm and are premultiplied a second
                        // time, which costs a shade of saturation on a
                        // path a dictionary popup barely has.
                        let a = u32::from(color.a());
                        if a == 0 {
                            return;
                        }
                        // Clip, don't trust: cosmic-text reports the
                        // glyph's ink box, which overhangs the wrap box
                        // on both sides and goes negative for a leading
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
                        // background, which is already there. The result
                        // keeps `rgb <= a` because each channel is
                        // monotone in a value that already did, so
                        // `from_rgba` cannot actually refuse it.
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
/// Glyph offsets are relative to their *buffer line*, and
/// `set_rich_text` splits the spans' text on line endings and strips
/// them, so anything mapping a glyph back to the text it came from has
/// to accumulate the lines' bases. Core's runs are single lines today;
/// this keeps the answer right if one ever is not.
#[derive(Default)]
struct LineBases {
    /// Bytes before the current line.
    base: usize,
    /// The buffer line it counts to.
    line: Option<usize>,
    /// That line's length in bytes.
    len: usize,
}

impl LineBases {
    /// The base for `line`, which must not precede the last one asked
    /// for: layout runs arrive top down.
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
/// DirectWrite hit-tests UTF-16 code-unit offsets natively; cosmic-text
/// is UTF-8 and its glyph clusters are byte ranges, so the conversion
/// lands here. Walking is the honest way to do it - a UTF-16 offset has
/// no arithmetic relation to a byte offset once the text leaves the BMP,
/// and Japanese text reaches into it (astral kanji, emoji in a gloss).
/// An offset past the end answers the end.
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

/// The span covering byte offset `at`, and its index.
///
/// Linear: a paragraph carries a handful of spans and a search
/// structure would cost more to build than it saves.
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
/// An offset that no glyph covers - past the end of the text, or inside
/// a cluster boundary core did not expect - answers a zero-width box at
/// the end of the last line rather than panicking or being skipped.
fn caret_box(buffer: &Buffer, spans: &[StyledSpan<'_>], utf16: u32) -> GlyphBox {
    let target = byte_offset(spans, utf16);
    // A run with no lines at all has no shaped height to report, so the
    // first span's own advance stands in.
    let empty = line_height(spans.first().map_or(0.0, |s| s.size));
    let mut end = GlyphBox { x: 0.0, y: 0.0, w: 0.0, h: empty };
    let mut bases = LineBases::default();
    for run in buffer.layout_runs() {
        let base = bases.advance(spans, &run);
        // The caret is as tall as the line it lands on, so a small span
        // beside a large one still gives a full-height hit target.
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

/// Does `family` render `text` as anything but tofu?
fn covers(fonts: &mut FontSystem, family: &str, text: &str) -> bool {
    // Wide enough that nothing wraps; the probe only cares about glyph
    // ids, but a wrap would not change them anyway.
    let buffer = TextEngine::shape(fonts, &[probe_span(text, family, 16.0)], 1024.0);
    let mut any = false;
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            // Glyph 0 is `.notdef` - the box the user sees when no face
            // in the fallback chain covered the character.
            if glyph.glyph_id == 0 {
                return false;
            }
            any = true;
        }
    }
    any
}

// ---- the Japanese-font probe ----

/// What kanji will look like on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JpFonts {
    /// A Japanese-capable family is installed and kanji resolve.
    Present { family: String },
    /// Kanji resolve, but only from a Chinese or Korean family, so Han
    /// unification hands out the wrong glyph shapes for Japanese.
    WrongVariant { family: String },
    /// No CJK coverage at all: kanji are tofu.
    Missing,
}

/// Family-name markers that mean "this family draws Japanese".
///
/// Substring matches, folded to lower case. Two shapes of evidence: an
/// explicit JP locale tag (`CJK JP`, ` JP`) and the names of the
/// families Linux distributions actually ship Japanese in. The bare
/// `gothic`/`mincho` entries are the Japanese type classes and catch the
/// long tail (`TakaoGothic`, `Sazanami Mincho`, distro-renamed IPA
/// builds); [`NON_JP`] and [`FALSE_GOTHIC`] veto them first.
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
/// Checked before [`JP_MARKERS`], so a name matching both loses: this is
/// what keeps `Noto Sans CJK SC` out of the JP bucket even though it
/// contains no JP tag but does contain `Sans`, and what keeps
/// `Hiragino Sans GB` (a Simplified Chinese face with a Japanese
/// vendor's name) and `Gothic A1` (Korean) out of it too. A name from
/// this table is also what a `WrongVariant` verdict points at.
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

/// Faces whose name carries "Gothic" for a reason that has nothing to do
/// with Japanese, and which cover no kanji at all: the American type
/// term (`Century Gothic` and the rest) and `Noto Sans Gothic`, which
/// draws Wulfila's Gothic *script* - `U+10330..U+1034A` and a handful of
/// combining marks, nothing else - and ships in the same `noto-fonts`
/// package every desktop already has. Without this table one of them
/// installed beside a Chinese CJK font would pass as Japanese and
/// suppress the warning the user needs, and the settings font combo
/// ([`jp_capable`]) would offer a family that can only draw tofu.
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
/// Matched against the last space-separated token, because that is the
/// only place they are unambiguous: `Sarasa Gothic SC` is Simplified
/// Chinese and `Sarasa Gothic J` is Japanese, and [`NON_JP`]'s infix
/// entries (`cjk sc`, `sans sc`) miss both. A Latin small-caps face -
/// `Alegreya SC` - trips this too, which costs nothing: it never
/// matched [`JP_MARKERS`], and [`classify`] prefers a recognised CJK
/// name when it has to point at one.
const LOCALE_TAGS: &[&str] = &["sc", "tc", "hc", "cn", "tw", "hk", "kr", "k"];

fn locale_tagged(lower: &str) -> bool {
    lower.rsplit(' ').next().is_some_and(|tag| LOCALE_TAGS.contains(&tag))
}

/// Which of [`classify`]'s buckets a family name falls in.
///
/// One lowercasing, one pass over the tables, and one place the order of
/// the vetoes is written down: [`classify`] needs all four answers and
/// [`jp_capable`] needs one of them, and a second copy of that order
/// would be a second definition of "Japanese font".
enum Bucket {
    /// A name the tables read as Japanese.
    Jp,
    /// A [`NON_JP`] name: recognised, and recognisably not Japanese.
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

/// Does this family name read as Japanese?
///
/// The per-name half of [`classify`], for the settings font combo
/// (ARCHITECTURE.md#settings-and-config: the combo is populated from
/// fontdb's JP-capable families). A name question only - there is no
/// shaping here, so unlike [`classify`] it cannot know what a face's
/// cmap holds. That is the right trade for a combo, which offers
/// candidates rather than passing a verdict on the machine.
pub fn jp_capable(family: &str) -> bool {
    matches!(bucket(family), Bucket::Jp)
}

/// The verdict for a set of installed families.
///
/// Pure, so the table above is testable without a font stack.
/// `kanji_covered` comes from actually shaping kanji
/// ([`TextEngine::probe`]) and outranks every name: a name table cannot
/// know what a face's cmap holds, so no coverage means [`JpFonts::Missing`]
/// however Japanese the installed names look.
pub fn classify(families: &[&str], kanji_covered: bool) -> JpFonts {
    if !kanji_covered {
        return JpFonts::Missing;
    }
    // Two buckets of "not Japanese", because they differ in how much
    // they are worth saying out loud: a family the table recognises can
    // be named in the warning, a merely locale-tagged one cannot be
    // trusted to be CJK at all.
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
    // Kanji resolved, so *something* covers them even when no name in
    // the tables claims it - a bundled pan-CJK face, say. Say so without
    // naming a family we did not recognise.
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
/// Goes to the daemon log and the tray, so it matches the house style of
/// the other degrade-visibly messages (`cursor:`, `config:`): one line,
/// lower case, no trailing period, and it names the package rather than
/// leaving the user to guess.
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

    /// Anything needing real glyph geometry needs a real Japanese face,
    /// and a CI runner carries almost no fonts. Those tests announce
    /// themselves out instead of failing.
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

    /// One run's measurement, through the seam.
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

    /// Does `family` ship a bold face?
    fn ships_bold(engine: &TextEngine, family: &str) -> bool {
        engine.fonts.db().faces().any(|face| {
            face.weight == Weight::BOLD
                && face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    /// A themed weight reaches the shaper.
    ///
    /// What core's per-role weights buy on Linux: a bold role has to
    /// resolve to the family's bold face, not be quietly shaped regular
    /// the way every run was before CSS theming. Skipped when the
    /// family ships one weight, since fontdb then answers with the
    /// nearest face it has and there is nothing to tell apart.
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

    /// The one invariant this whole module is built around: with
    /// no Japanese family named, kanji still resolve through
    /// cosmic-text's Han unification - and at locale `ja` that lands on
    /// the JP face, not the Simplified Chinese one. A regression here
    /// draws Chinese glyph variants for every kanji the popup shows and
    /// nothing else fails, which is why it is asserted rather than
    /// trusted.
    #[test]
    fn kanji_fall_back_to_the_japanese_face_and_never_the_chinese_one() {
        let Some(mut engine) = jp_engine() else { return };
        // A family that exists nowhere: only the script fallback can
        // answer, which is exactly the path the locale decides.
        let Some(face) = face_of(&mut engine, "\u{6f22}\u{5b57}", "chibipop-no-such-family") else {
            eprintln!("skipping: the font stack produced no glyphs at all");
            return;
        };
        let lower = face.to_ascii_lowercase();
        assert!(lower.contains("jp"), "kanji fell back to {face}, which is not a Japanese face");
        // Whole tags only: `NotoSansCJKjp-Regular` contains a bare "sc"
        // inside "SansCJK", so the Chinese and Korean faces have to be
        // named as the tags they carry.
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

    /// `Noto Sans Gothic` is the one that matters in practice: it draws
    /// the Gothic script, covers no kanji, and is already installed
    /// wherever `noto-fonts` is.
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

    /// Sarasa ships one family per locale and its tag is the last token,
    /// so the type-class markers alone cannot tell them apart.
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

    /// The safety property this test locks down: widening the seam
    /// must not move a number. Every assertion here is the arithmetic
    /// the one-string seam did - `lines × size × LINE_HEIGHT` for the
    /// height, the widest `line_w` for the width - so a single-span
    /// request that drifts fails here rather than in a golden.
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
            // The shaper's own running top, which is what it paints
            // from; the aggregate height is summed wide and rounded
            // once instead, so the two can differ by an ulp at eight
            // lines and the block stack still may not move.
            assert_eq!(pair[0].y + pair[0].h, pair[1].y, "a line starts where the last ended");
        }
        for line in &m.lines {
            assert_eq!(16.0 * LINE_HEIGHT, line.h);
            assert!(line.baseline > 0.0 && line.baseline < line.h, "{line:?}");
        }
        // One span, so one box per line, each starting at the margin.
        assert_eq!(n as usize, m.spans.len());
        for (i, b) in m.spans.iter().enumerate() {
            assert_eq!((0, i as u32), (b.span, b.line));
            assert_eq!(0.0, b.x);
            assert_eq!(m.lines[i].w, b.w, "the only span on a line fills it");
            assert_eq!(16.0 * LINE_HEIGHT, b.h);
        }
    }

    /// The whole point of the widening: two styles on one line, each
    /// with its own box, all hung off one baseline. Asserted against
    /// real cosmic-text output rather than a fake, because it is the
    /// shaper - not core - that decides a mixed line's height.
    #[test]
    fn spans_of_two_sizes_share_a_line_and_a_baseline() {
        let Some(mut engine) = jp_engine() else { return };
        // 漢 at body size, 字 half of it: wide enough that neither wraps.
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

        // Each span asks for its own advance; the line takes the max.
        assert_eq!(line_height(20.0), big.h);
        assert_eq!(line_height(10.0), small.h);
        assert_eq!(line_height(20.0), m.lines[0].h, "the taller span sets the line");
        assert_eq!(m.lines[0].h, m.metrics.h);
        // One baseline for the line, inside it, which is the whole
        // reason it is a required output of the seam.
        let base = m.lines[0].baseline;
        assert!(base > 0.0 && base < m.lines[0].h, "baseline {base} outside the line");
    }

    /// The walk hands one `Measured` to every element in a panel, so a
    /// measurer that appended instead of clearing would grow each
    /// element's geometry by every element before it - and the panel
    /// would still lay out, just wrong.
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

    /// The one place the real shaper meets the real walk: everything
    /// else drives `layout::scene` from a fake and this module from
    /// core's types, so nothing else would notice the two disagreeing.
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

    /// Drill-down probes a headword that ticket 07 may hand over as
    /// several styled spans, so an offset has to be counted over the
    /// run's whole text and not just the first span's.
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
        // 🍣 is astral (two UTF-16 units, four bytes), so 寿 starts at
        // UTF-16 offset 2 and byte offset 4. A naive offset-as-index
        // would land inside the emoji.
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
        // One guard row past the pixmap: a stride overrun writes here
        // instead of tripping a slice bound, so check it explicitly.
        let mut data = vec![0u8; W * (H + 1) * 4];
        let (front, guard) = data.split_at_mut(W * H * 4);
        for px in front.chunks_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 255]);
        }
        let before = front.to_vec();
        let mut target = PixmapMut::from_bytes(front, W as u32, H as u32).expect("pixmap");

        let draw = |target: &mut PixmapMut<'_>, engine: &mut TextEngine, origin| {
            // White on the black fill below, so ink is visible ink.
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

        // The edges: a glyph hanging off the bottom-right and one at a
        // negative origin both have to clip, not index out of the
        // buffer and not spill into the next row.
        draw(&mut target, &mut engine, (W as f32 - 3.0, H as f32 - 3.0));
        draw(&mut target, &mut engine, (-8.0, -8.0));
        draw(&mut target, &mut engine, (-4000.0, -4000.0));
        assert!(guard.iter().all(|&b| b == 0), "wrote past the pixmap");
    }
}
