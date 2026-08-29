//! The media store's vocabulary and its two codec halves.
//!
//! Image nodes are characters, not illustrations: 427 786 nodes in
//! `docs/research/dict-shapes.md` carry a `data.gaiji` marker and sit at
//! `height: 1em` in the middle of a definition, and 字通 averages more than
//! four per term row. So an asset has to be *sized* before the popup can
//! lay a line out, and **99 807 census image nodes declare neither `width`
//! nor `height`**. That is the whole reason this module exists.
//!
//! Two halves, at opposite ends of the pipeline:
//!
//! - [`probe`] runs once per asset at dictionary build time and reads the
//!   intrinsic size out of the **container header**. No pixels are decoded:
//!   a PNG's `IHDR`, a GIF's logical screen descriptor, a JPEG's `SOFn`, an
//!   AVIF's `ispe` property and an SVG's root element all carry the size in
//!   the first few hundred bytes. That is what keeps images out of the
//!   measurement seam entirely - the row already knows how big the thing
//!   is, so `layout` never touches a decoder.
//! - [`decode`] runs at paint time in the bin, behind that bin's own
//!   surface cache, and is the only place in this tree that turns encoded
//!   asset bytes into pixels.
//!
//! Reading the header rather than decoding is also what lets the build
//! support AVIF without an AVIF decoder: `ispe` is a container property, so
//! sizing an AVIF is box-walking, not entropy decoding. [`decode`] is
//! honest about the consequence and answers [`Undecodable::NoDecoder`] for
//! the formats this build cannot rasterize, which is a definite answer the
//! `alt`-text fallback can act on.

use std::fmt;

/// What names one asset: the dictionary that shipped it and the `path` its
/// image node declared, **verbatim**.
///
/// Verbatim matters. The path is a dictionary-authored string that only has
/// to round-trip, and normalising it would mean the key a scene element
/// carries and the key the build wrote could disagree. Archive lookup does
/// normalise, once, at extraction time - see `dict::build`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MediaKey {
    pub dict_id: i64,
    pub path: String,
}

impl MediaKey {
    pub fn new(dict_id: i64, path: impl Into<String>) -> MediaKey {
        MediaKey { dict_id, path: path.into() }
    }
}

impl fmt::Display for MediaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dict {} / {}", self.dict_id, self.path)
    }
}

/// The five formats `docs/research/dict-shapes.md` found across 30 image
/// dictionaries, and nothing else.
///
/// `.jpg` and `.jpeg` are one format; the census counts them separately
/// because it counts extensions, and this does not because it sniffs magic
/// bytes. A format outside this set is [`Unreadable::Unrecognised`]: no
/// media row, and the `alt`-text ladder takes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum MediaFormat {
    Png,
    Jpeg,
    /// Animated or not. The intrinsic size is the logical screen, which is
    /// also the canvas the first frame composites into.
    Gif,
    Svg,
    Avif,
}

impl MediaFormat {
    /// The `media.format` column's value.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaFormat::Png => "png",
            MediaFormat::Jpeg => "jpeg",
            MediaFormat::Gif => "gif",
            MediaFormat::Svg => "svg",
            MediaFormat::Avif => "avif",
        }
    }

    /// Reads the column back. `None` for a value this build does not know,
    /// which can only mean a database written by a newer one.
    pub fn parse(s: &str) -> Option<MediaFormat> {
        Some(match s {
            "png" => MediaFormat::Png,
            "jpeg" => MediaFormat::Jpeg,
            "gif" => MediaFormat::Gif,
            "svg" => MediaFormat::Svg,
            "avif" => MediaFormat::Avif,
            _ => return None,
        })
    }
}

impl fmt::Display for MediaFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An asset's intrinsic size in CSS pixels, recorded on the media row.
///
/// `aspect` is `width / height`, carried as its own column rather than
/// derived on read: an image that declares only one of `width`/`height` -
/// the `height: 1em` shape 字通 and 三省堂 both use - is sized by
/// multiplying, and deriving would put a division and a zero guard on a
/// path that runs tens of times per hover.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Intrinsic {
    pub format: MediaFormat,
    pub width: f32,
    pub height: f32,
    pub aspect: f32,
}

/// Why an asset got no media row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unreadable {
    /// The magic bytes match none of the five census formats.
    Unrecognised,
    /// The container was recognised and then did not hold a size: a
    /// truncated header, a zero dimension, or a JPEG with no frame header.
    Malformed(MediaFormat),
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreadable::Unrecognised => f.write_str("not one of png/jpeg/gif/svg/avif"),
            Unreadable::Malformed(fmt) => write!(f, "a {fmt} with no readable size"),
        }
    }
}

/// Bytes of an SVG scanned for its root element, and the window the
/// sniffer looks for `<svg` in.
///
/// One number for both, so a file whose root element the parser cannot
/// reach is also not claimed as an SVG by the sniffer. 16 KiB clears every
/// generator preamble a real asset carries - Illustrator's comment banner
/// is the long one - while bounding the `from_utf8_lossy` a probe does.
const SVG_WINDOW: usize = 16 << 10;

/// The largest dimension a real asset declares, by a wide margin.
///
/// A header is four bytes of attacker- or bug-controlled integer, and a
/// declared 4 294 967 295 x 1 would give ticket 12 an aspect ratio of four
/// billion to reason about. No dictionary asset in the census is anywhere
/// near 65 536 px, so refusing that is refusing corruption, not content.
const MAX_DIM: f32 = 65_536.0;

/// CSS Images 3's default object size, which is what a browser draws for a
/// replaced element with neither an intrinsic size nor an intrinsic ratio.
/// Only an SVG can reach it - every raster header states its size.
const DEFAULT_OBJECT: (f64, f64) = (300.0, 150.0);

/// The intrinsic size in an asset's container header.
///
/// Total over arbitrary bytes: every read is bounds-checked and every
/// container walk is bounded by the slice, so a truncated or hostile asset
/// answers [`Unreadable`] rather than panicking or looping. The build
/// records the answer and skips the asset on an error, so a corrupt file
/// costs one diagnostic line and never the build.
pub fn probe(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    match raster_magic(bytes) {
        Some(MediaFormat::Png) => png_size(bytes),
        Some(MediaFormat::Jpeg) => jpeg_size(bytes),
        Some(MediaFormat::Gif) => gif_size(bytes),
        Some(MediaFormat::Avif) => avif_size(bytes),
        // XML carries no magic number, so recognising an SVG *is* finding
        // its root element - and the size comes out of that same tag, so
        // the scan happens once here rather than once to sniff and again
        // to measure.
        Some(MediaFormat::Svg) | None => {
            svg_size(&svg_root(bytes).ok_or(Unreadable::Unrecognised)?)
        }
    }
}

/// The format, from magic bytes rather than from the file extension.
///
/// The census counts extensions because that is all a JSON node gives it;
/// the build has the bytes, and a dictionary that ships a PNG named `.jpg`
/// is a real thing.
pub fn sniff(bytes: &[u8]) -> Option<MediaFormat> {
    raster_magic(bytes).or_else(|| svg_root(bytes).map(|_| MediaFormat::Svg))
}

/// The four formats with a magic number. Never [`MediaFormat::Svg`].
fn raster_magic(bytes: &[u8]) -> Option<MediaFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(MediaFormat::Png);
    }
    // SOI plus the first byte of the next marker: two bytes alone match
    // too much.
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(MediaFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(MediaFormat::Gif);
    }
    if is_avif(bytes) {
        return Some(MediaFormat::Avif);
    }
    None
}

/// Validated, so the rest of this module cannot record a size ticket 12
/// would have to defend itself against.
fn sized(format: MediaFormat, w: f64, h: f64) -> Result<Intrinsic, Unreadable> {
    let (width, height) = (w as f32, h as f32);
    let sane = |v: f32| v.is_finite() && v > 0.0 && v <= MAX_DIM;
    if !sane(width) || !sane(height) {
        return Err(Unreadable::Malformed(format));
    }
    Ok(Intrinsic { format, width, height, aspect: width / height })
}

fn be16(b: &[u8]) -> u32 {
    u32::from(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

// ---- PNG ----

/// `IHDR` is mandated to be the first chunk, at a fixed offset: eight bytes
/// of signature, four of chunk length, four of chunk type, then the two
/// big-endian dimensions.
fn png_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let malformed = Unreadable::Malformed(MediaFormat::Png);
    let head = bytes.get(12..24).ok_or(malformed)?;
    if &head[0..4] != b"IHDR" {
        return Err(malformed);
    }
    sized(MediaFormat::Png, f64::from(be32(&head[4..8])), f64::from(be32(&head[8..12])))
}

// ---- GIF ----

/// The logical screen descriptor follows the six-byte signature, little
/// endian. It is the intrinsic size even for an animation whose first frame
/// is smaller: the frame composites into this canvas.
fn gif_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let head = bytes.get(6..10).ok_or(Unreadable::Malformed(MediaFormat::Gif))?;
    let le16 = |b: &[u8]| f64::from(u16::from_le_bytes([b[0], b[1]]));
    sized(MediaFormat::Gif, le16(&head[0..2]), le16(&head[2..4]))
}

// ---- JPEG ----

/// A frame header, `SOF0` through `SOF15`.
///
/// The three holes are not frame headers despite sitting in the same
/// marker range: `C4` is a Huffman table, `C8` a JPEG extension, `CC` an
/// arithmetic-coding conditioning table. Walking past them is what makes
/// this work on progressive and lossless JPEGs and not only on baseline.
fn is_frame_header(marker: u8) -> bool {
    (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
}

/// Walks the marker segments to the first frame header.
///
/// There is no fixed offset to read: JPEG allows any number of `APPn`,
/// comment and table segments before the frame, and real assets carry
/// EXIF and ICC blocks. The walk stops at `SOS`, because entropy-coded
/// scan data is not a segment stream and a file with no frame header
/// before its first scan has no readable size.
fn jpeg_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let malformed = Unreadable::Malformed(MediaFormat::Jpeg);
    let mut at = 2;
    loop {
        let head = bytes.get(at..at + 2).ok_or(malformed)?;
        if head[0] != 0xff {
            return Err(malformed);
        }
        let marker = head[1];
        at += 2;
        // A run of `ff` is padding before the real marker byte.
        if marker == 0xff {
            at -= 1;
            continue;
        }
        // `01` and the eight restart markers stand alone: no length word.
        if marker == 0x01 || (0xd0..=0xd9).contains(&marker) {
            continue;
        }
        if marker == 0xda {
            return Err(malformed);
        }
        let len = bytes.get(at..at + 2).map(be16).ok_or(malformed)? as usize;
        if len < 2 {
            return Err(malformed);
        }
        if is_frame_header(marker) {
            // Sample precision, then height, then width.
            let frame = bytes.get(at + 2..at + 7).ok_or(malformed)?;
            let (h, w) = (be16(&frame[1..3]), be16(&frame[3..5]));
            return sized(MediaFormat::Jpeg, f64::from(w), f64::from(h));
        }
        at += len;
    }
}

// ---- AVIF (ISO base media file format) ----

/// One ISOBMFF box: its four-character type and its payload.
///
/// Stops at the first box it cannot trust rather than clamping, so a
/// truncated file walks the boxes it does have and then ends. The
/// `size == 0` form ("to end of file") is accepted because a single-item
/// AVIF written by a streaming encoder uses it for `mdat`.
struct Boxes<'a> {
    rest: &'a [u8],
}

fn boxes(payload: &[u8]) -> Boxes<'_> {
    Boxes { rest: payload }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<(&'a [u8], &'a [u8])> {
        let all = self.rest;
        let head = all.get(..8)?;
        let (mut size, mut body_at) = (u64::from(be32(&head[0..4])), 8usize);
        if size == 1 {
            size = be64(all.get(8..16)?);
            body_at = 16;
        } else if size == 0 {
            size = all.len() as u64;
        }
        let size = usize::try_from(size).ok()?;
        if size < body_at || size > all.len() {
            return None;
        }
        self.rest = &all[size..];
        Some((&head[4..8], &all[body_at..size]))
    }
}

/// A box's payload, by type, at one level.
fn child<'a>(payload: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(payload).find(|(ty, _)| *ty == want).map(|(_, body)| body)
}

/// `ftyp` names AVIF as either the major brand or a compatible one. `avis`
/// is the image-sequence brand; it still carries a `meta` box describing
/// its primary item, so it sizes the same way.
fn is_avif(bytes: &[u8]) -> bool {
    let Some(ftyp) = boxes(bytes).next().filter(|(ty, _)| *ty == b"ftyp").map(|(_, b)| b) else {
        return false;
    };
    // Major brand, then the version word, then the compatible list - all
    // four-byte tags, so one chunked scan covers every slot.
    let tags = ftyp.as_chunks::<4>().0;
    tags.iter().enumerate().any(|(i, tag)| i != 1 && (tag == b"avif" || tag == b"avis"))
}

/// The size lives in an `ispe` **item property**, not in a header, so
/// finding it is a walk: `meta` holds `pitm` (which item is the picture),
/// `iprp/ipco` (the property list) and `ipma` (which properties belong to
/// which item).
///
/// The association walk is not pedantry. A real AVIF carries an alpha
/// auxiliary item and often a thumbnail, each with its own `ispe`, so
/// taking the first one can size the picture from its thumbnail. When
/// `pitm` or `ipma` is missing the first `ispe` is the fallback, which is
/// the right answer for the single-item files a dictionary converter
/// emits.
fn avif_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let malformed = Unreadable::Malformed(MediaFormat::Avif);
    let meta = child(bytes, b"meta").ok_or(malformed)?;
    // `meta` is a FullBox: one version byte and three flag bytes first.
    let meta = meta.get(4..).ok_or(malformed)?;

    let (mut primary, mut ipma, mut ipco) = (None, None, None);
    for (ty, body) in boxes(meta) {
        match ty {
            b"pitm" => primary = primary_item(body),
            b"ipma" => ipma = Some(body),
            b"iprp" => ipco = child(body, b"ipco"),
            _ => {}
        }
    }
    let ipco = ipco.ok_or(malformed)?;

    // Property index is 1-based over `ipco`'s children, in source order.
    let extents: Vec<(u16, u32, u32)> = boxes(ipco)
        .enumerate()
        .filter(|(_, (ty, _))| *ty == b"ispe")
        .filter_map(|(i, (_, body))| {
            let d = body.get(4..12)?;
            Some((u16::try_from(i + 1).ok()?, be32(&d[0..4]), be32(&d[4..8])))
        })
        .collect();

    let wanted = primary
        .zip(ipma)
        .and_then(|(id, ipma)| associated(ipma, id))
        .and_then(|idx| extents.iter().find(|(i, _, _)| idx.contains(i)).copied());
    let (_, w, h) = wanted.or_else(|| extents.first().copied()).ok_or(malformed)?;
    sized(MediaFormat::Avif, f64::from(w), f64::from(h))
}

/// `pitm`: the primary item's id, sixteen bits at version 0 and
/// thirty-two from version 1.
fn primary_item(body: &[u8]) -> Option<u32> {
    let version = *body.first()?;
    let id = body.get(4..)?;
    if version == 0 {
        Some(be16(id.get(..2)?))
    } else {
        Some(be32(id.get(..4)?))
    }
}

/// `ipma`: the property indices associated with one item.
///
/// Two independent width choices, which is why this is a parse and not an
/// offset: the item id is sixteen or thirty-two bits by *version*, and a
/// property index is seven or fifteen bits by the low *flag* bit.
fn associated(body: &[u8], item: u32) -> Option<Vec<u16>> {
    let version = *body.first()?;
    let wide_index = body.get(3)? & 1 == 1;
    let mut at = 4;
    let count = be32(body.get(at..at + 4)?);
    at += 4;
    for _ in 0..count {
        let id = if version == 0 {
            let v = be16(body.get(at..at + 2)?);
            at += 2;
            v
        } else {
            let v = be32(body.get(at..at + 4)?);
            at += 4;
            v
        };
        let links = usize::from(*body.get(at)?);
        at += 1;
        let step = if wide_index { 2 } else { 1 };
        let block = body.get(at..at + links * step)?;
        at += links * step;
        if id != item {
            continue;
        }
        return Some(
            block
                .chunks_exact(step)
                .map(|c| {
                    // The high bit of the first byte is `essential`.
                    if wide_index {
                        u16::from_be_bytes([c[0] & 0x7f, c[1]])
                    } else {
                        u16::from(c[0] & 0x7f)
                    }
                })
                .collect(),
        );
    }
    None
}

// ---- SVG ----

/// The root element's opening tag, as text.
///
/// XML has no magic number, so this doubles as the sniffer: bytes that
/// contain an `<svg` element start within [`SVG_WINDOW`] are an SVG and
/// bytes that do not are not. Lossy decoding is deliberate - the tag is
/// ASCII, and a Latin-1 comment in the preamble must not make the file
/// unreadable.
fn svg_root(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(SVG_WINDOW)];
    let text = String::from_utf8_lossy(head);
    let at = text
        .match_indices("<svg")
        .find(|(at, _)| match text[at + 4..].chars().next() {
            // `<svgfoo` is a different element.
            None | Some('>') | Some('/') => true,
            Some(c) => c.is_ascii_whitespace(),
        })
        .map(|(at, _)| at)?;
    let tag = &text[at..];
    let end = tag.find('>')?;
    Some(tag[..end].to_string())
}

/// One attribute's value from an opening tag.
///
/// Whole-name matching, so `stroke-width` never answers a `width` probe,
/// and case-sensitive, because an SVG referenced by `<img src>` is parsed
/// as XML - which is also why `viewBox` keeps its capital B.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    loop {
        let at = rest.find(name)?;
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].trim_start();
        let bounded = at > 0 && before.is_some_and(|c| c.is_ascii_whitespace());
        if bounded && after.starts_with('=') {
            let value = after[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = value[1..].find(quote)?;
                return Some(value[1..1 + end].to_string());
            }
            // Unquoted: XML forbids it, but a hand-written file is a
            // hand-written file. Take the run up to whitespace.
            let end = value.find(char::is_whitespace).unwrap_or(value.len());
            return Some(value[..end].to_string());
        }
        rest = &rest[at + name.len()..];
    }
}

/// A CSS length in pixels.
///
/// `None` for a percentage and for the font-relative units, because both
/// size against a containing box the asset does not have and therefore
/// name no *intrinsic* size. Absolute units convert at CSS's own fixed
/// ratios, where an inch is 96 pixels by definition.
fn css_px(value: &str) -> Option<f64> {
    let value = value.trim();
    let numeric = |c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E');
    let cut = value.find(|c: char| !numeric(c)).unwrap_or(value.len());
    let number: f64 = value[..cut].parse().ok()?;
    let scale = match value[cut..].trim().to_ascii_lowercase().as_str() {
        "" | "px" => 1.0,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 9.6 / 2.54,
        "q" => 96.0 / 2.54 / 40.0,
        _ => return None,
    };
    Some(number * scale)
}

/// `viewBox`'s third and fourth numbers - the ratio when the root element
/// declares no size, which is the common shape for a gaiji SVG.
fn view_box(value: &str) -> Option<(f64, f64)> {
    let mut nums = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .skip(2)
        .map(str::parse::<f64>);
    let w = nums.next()?.ok()?;
    let h = nums.next()?.ok()?;
    (w > 0.0 && h > 0.0).then_some((w, h))
}

/// CSS's own resolution order for a replaced element: a declared size
/// wins, one declared length plus a ratio gives the other, a ratio alone
/// is the size, and nothing at all is the default object size.
///
/// The last arm is why an SVG never fails to size. It is not an invented
/// number - it is what a browser draws for an intrinsic-less replaced
/// element - so the media row's promise holds: a row exists, the bytes are
/// there, and the size is the one the asset would render at.
fn svg_size(tag: &str) -> Result<Intrinsic, Unreadable> {
    let w = attr(tag, "width").as_deref().and_then(css_px);
    let h = attr(tag, "height").as_deref().and_then(css_px);
    let ratio = attr(tag, "viewBox").as_deref().and_then(view_box);
    let (dw, dh) = DEFAULT_OBJECT;
    let (w, h) = match (w, h, ratio) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((vw, vh))) => (w, w * vh / vw),
        (None, Some(h), Some((vw, vh))) => (h * vw / vh, h),
        (None, None, Some((vw, vh))) => (vw, vh),
        (Some(w), None, None) => (w, dh),
        (None, Some(h), None) => (dw, h),
        (None, None, None) => (dw, dh),
    };
    sized(MediaFormat::Svg, w, h)
}

// ---- the paint-time half ----

/// One decoded asset: premultiplied RGBA8, top-down, `w * h * 4` bytes.
///
/// Premultiplied RGBA8 is tiny-skia's own pixel layout, so the Linux panel
/// blends a surface with no conversion at all; the Windows adapter swaps
/// two channels once per key when it builds its Direct2D bitmap. That is
/// the cheaper way round, because Linux converts per frame and Windows
/// per asset.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Why a decode produced no pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undecodable {
    /// This build carries no decoder for the format. A definite answer, not
    /// a failure: ticket 12's `alt`-text ladder acts on it, and the media
    /// row is still there with the right intrinsic size, so the line the
    /// image sits on is laid out the same either way.
    NoDecoder(MediaFormat),
    /// A decoder ran and refused the bytes.
    Broken(MediaFormat),
}

impl fmt::Display for Undecodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undecodable::NoDecoder(fmt) => write!(f, "this build decodes no {fmt}"),
            Undecodable::Broken(fmt) => write!(f, "undecodable {fmt}"),
        }
    }
}

/// Why an asset has no pixels to paint.
///
/// Every arm is a fallback case and none of them is a frame-losing error: a
/// dictionary's broken asset costs the popup its `alt` text, then a
/// placeholder box, and never a hole in a word. The arms differ only in
/// what a diagnostic should say.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Missing {
    /// No media row. Either the archive shipped nothing at that path or the
    /// build could read no intrinsic size out of what it did ship.
    NotStored,
    /// Stored and sized, and not paintable by this build.
    Undecodable(Undecodable),
    /// The store itself would not answer. Carries the message, so a bin
    /// logs a real database fault once instead of swallowing it.
    Unavailable(String),
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Missing::NotStored => f.write_str("no media row"),
            Missing::Undecodable(why) => write!(f, "{why}"),
            Missing::Unavailable(why) => write!(f, "unreadable media store: {why}"),
        }
    }
}

/// Asset bytes to pixels, with the format the media row already recorded
/// rather than a second sniff.
///
/// PNG only, and deliberately: PNG is the one image codec already resident
/// in this tree (`src/image.rs` encodes with it, the Linux tray decodes
/// with it), so it costs no dependency. JPEG, GIF, SVG and AVIF each need
/// a decoder the workspace does not carry, and pulling four of them in
/// ahead of the ticket that paints images would be four pins chosen
/// blind. They answer [`Undecodable::NoDecoder`] until ticket 12 chooses
/// them against a real paint path.
pub fn decode(format: MediaFormat, bytes: &[u8]) -> Result<Surface, Undecodable> {
    if format != MediaFormat::Png {
        return Err(Undecodable::NoDecoder(format));
    }
    decode_png(bytes).ok_or(Undecodable::Broken(MediaFormat::Png))
}

/// Palette, grey and 16-bit PNGs all normalise to eight bits with an alpha
/// channel, so this only has to widen grey to RGB and premultiply.
fn decode_png(bytes: &[u8]) -> Option<Surface> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut raw = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut raw).ok()?;
    let (w, h) = (info.width, info.height);
    let pixels = usize::try_from(w).ok()?.checked_mul(usize::try_from(h).ok()?)?;
    let lanes = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        // `EXPAND` turns a palette into colour, so this cannot arrive.
        png::ColorType::Indexed => return None,
    };
    raw.truncate(info.buffer_size());
    if raw.len() != pixels.checked_mul(lanes)? {
        return None;
    }

    let mut rgba = Vec::with_capacity(pixels * 4);
    for px in raw.chunks_exact(lanes) {
        let (r, g, b, a) = match lanes {
            4 => (px[0], px[1], px[2], px[3]),
            3 => (px[0], px[1], px[2], 255),
            2 => (px[0], px[0], px[0], px[1]),
            _ => (px[0], px[0], px[0], 255),
        };
        let mul = |c: u8| (u32::from(c) * u32::from(a) / 255) as u8;
        rgba.extend_from_slice(&[mul(r), mul(g), mul(b), a]);
    }
    Some(Surface { w, h, rgba })
}

#[cfg(test)]
mod tests;
