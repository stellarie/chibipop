//! The Linux text stack: cosmic-text behind core's `TextMeasure`, plus
//! the paint half and the startup Japanese-font probe (ADR-0004).
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
//! [`shape`], so a run is never wrapped one way and painted another
//! (ADR-0004). Windows earns the same property by routing both through
//! one `IDWriteTextLayout`; the twin here is
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

use chibipop::ui::layout::{GlyphBox, MeasureError, MeasureRun, Metrics, TextMeasure};
use cosmic_text::{
    fontdb, Attrs, Buffer, Color, Family, FontSystem, Metrics as CosmicMetrics, Shaping, Style,
    SwashCache, Weight, Wrap,
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
    /// popup re-shapes on every paint (ADR-0004) and this is what makes
    /// that free after the first frame.
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
        // doc and ADR-0004 ("Fractional scale and fonts"). The default
        // arm of cosmic-text's `han_unification` is Simplified Chinese,
        // so a system locale of `en-US` would silently render kanji with
        // Chinese glyph variants.
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
    /// An associated function, not a method, so a caller can hand it
    /// `&mut self.fonts` while still holding `&self.family`.
    fn shape(fonts: &mut FontSystem, run: Shaped<'_>) -> Buffer {
        let size = run.size.max(MIN_SIZE);
        let mut buffer = Buffer::new_empty(CosmicMetrics::new(size, size * LINE_HEIGHT));
        // No height bound: shape every wrapped line, not just the ones
        // that would fit a viewport. Core culls off-panel runs itself
        // and needs the full metrics to decide.
        //
        // The width clamp is the same one DirectWrite gets on Windows: a
        // measurer that cannot wrap at zero clamps itself, and the scene
        // still reports the width it asked for.
        buffer.set_size(Some(run.max_w.max(1.0)), None);
        // Words first, glyphs when a "word" cannot fit alone. Japanese
        // has no spaces, so the segmenter treats runs of ideographs as
        // their own words and this behaves like CJK line breaking; the
        // glyph fallback is what keeps a long Latin headword inside a
        // narrow panel.
        buffer.set_wrap(Wrap::WordOrGlyph);
        // Weight and style are the theme's, per role (CSS theming):
        // fontdb weights are the same 100-900 numbers DirectWrite
        // takes, so the scene's number travels unconverted. A family
        // with no bold or italic face still shapes - fontdb picks the
        // nearest weight it has, and cosmic-text does not synthesize -
        // so a missing face costs the emphasis, never the run.
        let attrs = Attrs::new()
            .family(Family::Name(run.family))
            .weight(Weight(run.weight))
            .style(if run.italic { Style::Italic } else { Style::Normal });
        buffer.set_text(run.text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        buffer
    }
}

/// What [`TextEngine::shape`] needs.
///
/// A `MeasureRun` and a `DrawRun` differ only in where the family comes
/// from - the scene's theme when measuring, the resolved family when
/// painting - so both funnel through this and hit one shaping path.
#[derive(Debug, Clone, Copy)]
struct Shaped<'a> {
    text: &'a str,
    family: &'a str,
    size: f32,
    weight: u16,
    italic: bool,
    max_w: f32,
}

impl<'a> Shaped<'a> {
    /// One run to measure, as the scene
    /// named it.
    fn measuring(run: MeasureRun<'a>) -> Shaped<'a> {
        Shaped {
            text: run.text,
            family: run.font,
            size: run.size,
            weight: run.weight,
            italic: run.italic,
            max_w: run.max_w,
        }
    }

    /// One run to paint, in the family
    /// the config resolved to.
    fn painting(run: DrawRun<'a>, family: &'a str) -> Shaped<'a> {
        Shaped {
            text: run.text,
            family,
            size: run.size,
            weight: run.weight,
            italic: run.italic,
            max_w: run.max_w,
        }
    }

    /// One coverage probe.
    ///
    /// Regular and upright: a probe
    /// asks which face answers for a
    /// character, and a family's bold
    /// or italic face - if it has one -
    /// covers what its regular one
    /// does.
    fn probing(text: &'a str, family: &'a str, size: f32, max_w: f32) -> Shaped<'a> {
        Shaped { text, family, size, weight: Weight::NORMAL.0, italic: false, max_w }
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
    fn measure(&mut self, run: MeasureRun<'_>) -> Result<Metrics, MeasureError> {
        let buffer = TextEngine::shape(&mut self.fonts, Shaped::measuring(run));
        let mut w = 0.0f32;
        let mut lines = 0u32;
        for line in buffer.layout_runs() {
            w = w.max(line.line_w);
            lines += 1;
        }
        // An empty string is one empty line, not zero: core stacks the
        // gap after it either way.
        let lines = lines.max(1);
        // Stackable by construction - a whole number of line advances,
        // never an ink box. Core's walk adds these up.
        Ok(Metrics { w, h: lines as f32 * line_height(run.size), lines })
    }

    fn caret_boxes(
        &mut self,
        run: MeasureRun<'_>,
        at: &[u32],
        out: &mut Vec<GlyphBox>,
    ) -> Result<(), MeasureError> {
        let buffer = TextEngine::shape(&mut self.fonts, Shaped::measuring(run));
        let h = line_height(run.size);
        // Exactly one box per offset, in order: core zips these 1:1 with
        // the kanji of a headword to build per-character hit targets, so
        // a skipped offset would silently shift every target after it.
        for &offset in at {
            out.push(caret_box(&buffer, run.text, offset, h));
        }
        Ok(())
    }
}

impl PanelText for TextEngine {
    fn draw_run(&mut self, run: DrawRun<'_>, target: &mut PixmapMut<'_>) {
        let shaped = Shaped::painting(run, &self.family);
        let mut buffer = TextEngine::shape(&mut self.fonts, shaped);
        let (r, g, b) = run.color;
        // The glyph raster is already snapped to the pixel grid by
        // cosmic-text, so the wrap box's own origin is too; a fractional
        // pen would only smear the hinting.
        let (ox, oy) = (round(run.origin.0), round(run.origin.1));
        let (w, h) = (target.width() as i32, target.height() as i32);
        let stride = target.width() as usize;
        let px = target.pixels_mut();
        buffer.draw(
            &mut self.fonts,
            &mut self.swash,
            Color::rgb(r, g, b),
            |gx, gy, gw, gh, color| {
                // cosmic-text lays a buffer out from its own (0, 0); the
                // wrap box's place in the buffer is the run's.
                let (gx, gy) = (gx.saturating_add(ox), gy.saturating_add(oy));
                // Straight alpha: for a mask glyph - everything our
                // faces produce - cosmic-text hands back the base RGB
                // with the coverage in alpha. Colour bitmaps (emoji)
                // come through the same arm and are premultiplied a
                // second time, which costs a shade of saturation on a
                // path a dictionary popup barely has.
                let a = u32::from(color.a());
                if a == 0 {
                    return;
                }
                let (r, g, b) = (u32::from(color.r()), u32::from(color.g()), u32::from(color.b()));
                let inv = 255 - a;
                // Clip, don't trust: cosmic-text reports the glyph's ink
                // box, which overhangs the wrap box on both sides and
                // goes negative for a leading side bearing.
                for y in gy.max(0)..gy.saturating_add(gh as i32).min(h) {
                    let row = y as usize * stride;
                    for x in gx.max(0)..gx.saturating_add(gw as i32).min(w) {
                        let i = row + x as usize;
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
                    }
                }
            },
        );
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

/// The byte offset `utf16` names in `text`.
///
/// DirectWrite hit-tests UTF-16 code-unit offsets natively; cosmic-text
/// is UTF-8 and its glyph clusters are byte ranges, so the conversion
/// lands here. Walking is the honest way to do it - a UTF-16 offset has
/// no arithmetic relation to a byte offset once the text leaves the BMP,
/// and Japanese text reaches into it (astral kanji, emoji in a gloss).
/// An offset past the end answers the end.
fn byte_offset(text: &str, utf16: u32) -> usize {
    let mut units = 0u32;
    for (byte, ch) in text.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += ch.len_utf16() as u32;
    }
    text.len()
}

/// The box of the cluster covering UTF-16 offset `utf16`.
///
/// An offset that no glyph covers - past the end of the text, or inside
/// a cluster boundary core did not expect - answers a zero-width box at
/// the end of the last line rather than panicking or being skipped.
fn caret_box(buffer: &Buffer, text: &str, utf16: u32, h: f32) -> GlyphBox {
    let target = byte_offset(text, utf16);
    let mut end = GlyphBox { x: 0.0, y: 0.0, w: 0.0, h };
    // Glyph offsets are relative to their *buffer line*, and `set_text`
    // splits on line endings, so a run carrying a newline needs its
    // lines' bases accumulated. Core's runs are single lines today; this
    // keeps the answer right if one ever is not.
    let mut base = 0usize;
    let mut line = usize::MAX;
    let mut line_len = 0usize;
    for run in buffer.layout_runs() {
        if run.line_i != line {
            if line != usize::MAX {
                base += line_len;
                base += ending_len(&text[base.min(text.len())..]);
            }
            line = run.line_i;
            line_len = run.text.len();
        }
        end = GlyphBox { x: run.line_w, y: run.line_top, w: 0.0, h };
        for glyph in run.glyphs {
            if (base + glyph.start..base + glyph.end).contains(&target) {
                return GlyphBox { x: glyph.x, y: run.line_top, w: glyph.w, h };
            }
        }
    }
    end
}

/// The line ending `set_text` stripped at the head of `rest`.
fn ending_len(rest: &str) -> usize {
    if rest.starts_with("\r\n") {
        2
    } else if rest.starts_with('\n') || rest.starts_with('\r') {
        1
    } else {
        0
    }
}

/// Does `family` render `text` as anything but tofu?
fn covers(fonts: &mut FontSystem, family: &str, text: &str) -> bool {
    // Wide enough that nothing wraps; the probe only cares about glyph
    // ids, but a wrap would not change them anyway.
    let buffer = TextEngine::shape(fonts, Shaped::probing(text, family, 16.0, 1024.0));
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
/// (ADR-0005: the combo is populated from fontdb's JP-capable
/// families). A name question only - there is no shaping here, so
/// unlike [`classify`] it cannot know what a face's cmap holds. That is
/// the right trade for a combo, which offers candidates rather than
/// passing a verdict on the machine.
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

    /// The family the Linux theme default names (ADR-0004).
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

    fn run<'a>(text: &'a str, size: f32, max_w: f32) -> MeasureRun<'a> {
        MeasureRun { text, font: JP, size, weight: 400, italic: false, max_w }
    }

    /// The same run, in `weight`.
    fn heavy<'a>(text: &'a str, size: f32, max_w: f32, weight: u16) -> MeasureRun<'a> {
        MeasureRun { weight, ..run(text, size, max_w) }
    }

    /// The face a shaped run's glyphs came from, as fontdb names it.
    fn face_of_shape(engine: &mut TextEngine, shaped: Shaped<'_>) -> Option<String> {
        let buffer = TextEngine::shape(&mut engine.fonts, shaped);
        let id = buffer.layout_runs().flat_map(|line| line.glyphs.iter()).map(|g| g.font_id).next()?;
        engine.fonts.db().face(id).map(|face| face.post_script_name.clone())
    }

    fn face_of(engine: &mut TextEngine, text: &str, family: &str) -> Option<String> {
        face_of_shape(engine, Shaped::probing(text, family, 20.0, 400.0))
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
        let regular = face_of_shape(&mut engine, Shaped::measuring(run(PROBE_TEXT, 20.0, 400.0)));
        let bold =
            face_of_shape(&mut engine, Shaped::measuring(heavy(PROBE_TEXT, 20.0, 400.0, 700)));
        assert!(regular.is_some(), "the probe text must shape");
        assert_ne!(regular, bold, "a bold role must not shape in the regular face");
    }

    /// The one invariant ADR-0004 built this whole module around: with
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

    #[test]
    fn an_empty_run_measures_no_width_and_exactly_one_line() {
        let Some(mut engine) = jp_engine() else { return };
        let m = engine.measure(run("", 16.0, 200.0)).expect("shapeable");
        assert_eq!(0.0, m.w);
        assert_eq!(1, m.lines);
        assert_eq!(16.0 * LINE_HEIGHT, m.h);
    }

    #[test]
    fn a_long_japanese_run_wraps_and_its_height_is_whole_lines() {
        let Some(mut engine) = jp_engine() else { return };
        let text = "\u{8f9e}\u{66f8}\u{306e}\u{8aac}\u{660e}\u{6587}\u{3092}\u{72ed}\u{3044}\
                    \u{5e45}\u{3067}\u{6298}\u{308a}\u{8fd4}\u{3059}\u{305f}\u{3081}\u{306e}\
                    \u{9577}\u{3044}\u{6587}\u{7ae0}";
        let m = engine.measure(run(text, 16.0, 60.0)).expect("shapeable");
        assert!(m.lines > 1, "{} lines at max_w 60", m.lines);
        assert_eq!(m.lines as f32 * 16.0 * LINE_HEIGHT, m.h, "runs stack by whole lines");
        assert!(m.w <= 60.0, "wrapped width {} exceeds the wrap box", m.w);
    }

    #[test]
    fn a_zero_wrap_width_clamps_instead_of_panicking() {
        let Some(mut engine) = jp_engine() else { return };
        let m = engine.measure(run(PROBE_TEXT, 16.0, 0.0)).expect("shapeable");
        assert!(m.lines >= 1);
        assert!(m.h > 0.0);
    }

    #[test]
    fn caret_boxes_answer_one_ordered_box_per_offset() {
        let Some(mut engine) = jp_engine() else { return };
        // 漢字辞書 - four BMP kanji, so one UTF-16 unit each.
        let text = "\u{6f22}\u{5b57}\u{8f9e}\u{66f8}";
        let mut out = Vec::new();
        engine
            .caret_boxes(run(text, 20.0, 400.0), &[0, 1, 2, 3], &mut out)
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

    #[test]
    fn an_offset_past_the_text_answers_a_zero_width_box_rather_than_nothing() {
        let Some(mut engine) = jp_engine() else { return };
        let mut out = Vec::new();
        engine
            .caret_boxes(run(PROBE_TEXT, 20.0, 400.0), &[0, 99], &mut out)
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
        assert_eq!(4, byte_offset(text, 2));
        let mut out = Vec::new();
        engine.caret_boxes(run(text, 20.0, 400.0), &[2], &mut out).expect("shapeable");
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
            engine.draw_run(
                DrawRun {
                    text: PROBE_TEXT,
                    size: 20.0,
                    weight: 400,
                    italic: false,
                    max_w: 200.0,
                    color: (255, 255, 255),
                    origin,
                },
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
