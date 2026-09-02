//! The media store defines asset types and two codec stages.
//!
//! A Dictionary Entry treats an image node as a character, not an illustration.
//! `docs/research/dict-shapes.md` reports 427 786 nodes with a
//! `data.gaiji` marker. These nodes use `height: 1em` inside a definition.
//! The Dictionary 字通 uses more than four such nodes in each term row. The
//! layout pass needs an asset size before it places a line that contains the
//! asset. The census reports 99 807 image nodes with neither `width` nor
//! `height`. This module gets those sizes.
//!
//! [`probe`] runs once for each asset during dictionary build. It reads the
//! intrinsic size from the container header and does not decode pixels. Each
//! format stores its size in the first few hundred bytes:
//! - a PNG, in its `IHDR` chunk
//! - a GIF, in its logical screen descriptor
//! - a JPEG, in its `SOFn` marker
//! - an AVIF, in its `ispe` property
//! - an SVG, in its root element
//!
//! The media row records this size. The TextMeasure seam then avoids a decoder,
//! and `layout` avoids one as well.
//! - [`decode`] runs when a platform bin paints an asset. The decoded-surface
//!   cache of that platform bin holds its result. This function alone changes
//!   encoded asset bytes into pixels.
//!   Each census format has one rasterizer. A raster asset gets pixels from its
//!   bytes. An SVG is a vector without pixels until the layout pass resolves a
//!   box. The layout pass then gives that size to [`decode`].
//!
//! [`probe`] gets an asset size from its header and does not decode the asset.
//! This design let the build support AVIF before this module had an AVIF
//! decoder. The `ispe` property is inside the container, so [`probe`] reads
//! only container boxes. It does not decode entropy-coded data. [`decode`]
//! uses the same box pass to apply a size limit. It rejects a hostile size
//! before code allocates an AV1 frame.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::Hash;

/// A `MediaKey` names one asset. It contains the Dictionary that supplied the
/// asset and the exact `path` text from its image node.
///
/// The system must reuse the exact text that the Dictionary author wrote. A
/// normalized path can differ between a scene element key and the key that
/// the build wrote. The archive lookup normalizes each path once during
/// extraction. See `dict::build`.

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

/// The five formats that `docs/research/dict-shapes.md` found across 30 image
/// dictionaries. The census found no other formats.
///
/// `.jpg` and `.jpeg` use one format here. The census counts them separately
/// as file extensions. This code uses magic bytes instead. A format outside
/// this set produces [`Unreadable::Unrecognised`]. The build writes no media
/// row, and the `alt` text ladder acts.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum MediaFormat {
    Png,
    Jpeg,
/// The logical screen gives the intrinsic size for an animated or static GIF.
/// The first frame uses this screen as its composite canvas.
    Gif,
    Svg,
    Avif,
}

impl MediaFormat {
    /// Gets the value in the `media.format` column.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaFormat::Png => "png",
            MediaFormat::Jpeg => "jpeg",
            MediaFormat::Gif => "gif",
            MediaFormat::Svg => "svg",
            MediaFormat::Avif => "avif",
        }
    }

    /// Reads a value from the column. Returns `None` for an unknown value. Only a
    /// newer build can write an unknown database value.
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

/// The intrinsic size of an asset in CSS pixels. The media row stores this size.
///
/// `aspect` is its own column and equals `width / height`. The code does not
/// calculate it when it reads a row. An image can declare `width` alone or
/// `height` alone. This shape occurs with `height: 1em`, which 字通 and 三省堂
/// use. The code uses `aspect` to calculate the absent side. A calculation
/// during each hover would add a division and a zero guard.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Intrinsic {
    pub format: MediaFormat,
    pub width: f32,
    pub height: f32,
    pub aspect: f32,
}

/// Why the build wrote no media row for an asset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unreadable {
    /// The magic bytes do not match any of the five census formats.
    Unrecognised,
    /// The build recognized the container, but the container has no size. Causes
    /// include a truncated header, a zero dimension, or a JPEG with no frame
    /// header.
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

/// The byte limit for the SVG root element and the `<svg` sniff window.
///
/// One limit gives the parser and sniffer the same reach. 16 KiB includes
/// every generator preamble in the asset set. The Illustrator comment banner
/// is the longest preamble. This limit also sets the `from_utf8_lossy` input
/// for a probe.
const SVG_WINDOW: usize = 16 << 10;

/// The maximum valid asset dimension.
///
/// A header contains a four-byte integer that a defect or an attacker can
/// control. A value of 4 294 967 295 x 1 would give the layout pass an aspect
/// ratio of four billion. No Dictionary asset in the census nears 65 536 px.
/// This limit rejects corrupt data, not valid content.
const MAX_DIM: f32 = 65_536.0;

/// The default object size from CSS Images 3.
///
/// A browser uses this size for a replaced element with no intrinsic size or
/// ratio. Only an SVG can use it because each raster header states its size.
const DEFAULT_OBJECT: (f64, f64) = (300.0, 150.0);

/// Gets the intrinsic size from an asset container header.
///
/// This function accepts every byte sequence. Bounds checks protect every read,
/// and each container pass stays inside the slice. A truncated or hostile asset
/// returns [`Unreadable`] rather than a panic or an infinite loop. On an error,
/// the build records the result and skips the asset. A corrupt file produces one
/// diagnostic line and does not stop the build.
pub fn probe(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    match raster_magic(bytes) {
        Some(MediaFormat::Png) => png_size(bytes),
        Some(MediaFormat::Jpeg) => jpeg_size(bytes),
        Some(MediaFormat::Gif) => gif_size(bytes),
        Some(MediaFormat::Avif) => avif_size(bytes),
        // XML has no magic number. The `<svg` root element therefore identifies
        // the format and supplies the size. This pass gets both results in one scan.
        Some(MediaFormat::Svg) | None => {
            svg_size(&svg_root(bytes).ok_or(Unreadable::Unrecognised)?)
        }
    }
}

/// Gets the format from magic bytes, not from the file extension.
///
/// The census uses extensions because a JSON node has no bytes. The build has
/// bytes, and some Dictionaries supply a PNG with a `.jpg` name.
pub fn sniff(bytes: &[u8]) -> Option<MediaFormat> {
    raster_magic(bytes).or_else(|| svg_root(bytes).map(|_| MediaFormat::Svg))
}

/// Gets one of the four formats with magic numbers. This function never returns
/// [`MediaFormat::Svg`].
fn raster_magic(bytes: &[u8]) -> Option<MediaFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(MediaFormat::Png);
    }
    // Require the JPEG start-of-image bytes and the first byte of the next
    // marker. The first two bytes alone match too many files.
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

/// Accepts only a size that the layout pass can process. This check keeps
/// invalid sizes out of the media row.
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

/// PNG places `IHDR` at a fixed offset as its first chunk. The fields are the
/// eight-byte signature, chunk length, chunk type, and two big-endian dimensions.
fn png_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let malformed = Unreadable::Malformed(MediaFormat::Png);
    let head = bytes.get(12..24).ok_or(malformed)?;
    if &head[0..4] != b"IHDR" {
        return Err(malformed);
    }
    sized(MediaFormat::Png, f64::from(be32(&head[4..8])), f64::from(be32(&head[8..12])))
}

// ---- GIF ----

/// The six-byte signature precedes the little-endian logical screen descriptor.
/// This descriptor gives intrinsic size when the first animation frame is
/// smaller. The frame composites onto this canvas.
fn gif_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let head = bytes.get(6..10).ok_or(Unreadable::Malformed(MediaFormat::Gif))?;
    let le16 = |b: &[u8]| f64::from(u16::from_le_bytes([b[0], b[1]]));
    sized(MediaFormat::Gif, le16(&head[0..2]), le16(&head[2..4]))
}

// ---- JPEG ----

/// Identifies a frame header from `SOF0` through `SOF15`.
///
/// Three markers in this range are not frame headers. `C4` is a Huffman table.
/// `C8` is a JPEG extension. `CC` is a table for arithmetic code conditions.
/// The parser skips these markers for progressive, lossless, and baseline JPEGs.
fn is_frame_header(marker: u8) -> bool {
    (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
}

/// Reads marker segments until the first frame header.
///
/// JPEG permits any number of `APPn`, comment, and table segments before the
/// frame. Real assets can contain EXIF and ICC blocks. The parser stops at
/// `SOS`. Entropy-coded scan data is not a segment stream. A file without a
/// frame header before its first scan has no readable size.
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
        // A sequence of `ff` bytes pads the data before the marker byte.
        if marker == 0xff {
            at -= 1;
            continue;
        }
        // `01` and the eight restart markers have no length word.
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
            // The frame fields are sample precision, height, and width.
            let frame = bytes.get(at + 2..at + 7).ok_or(malformed)?;
            let (h, w) = (be16(&frame[1..3]), be16(&frame[3..5]));
            return sized(MediaFormat::Jpeg, f64::from(w), f64::from(h));
        }
        at += len;
    }
}

// ---- AVIF (ISO base media file format) ----

/// One ISOBMFF box with a four-character type and a payload.
///
/// The parser stops at the first box it cannot trust. It does not clamp a box
/// size. A truncated file therefore supplies complete boxes before that point.
/// The parser accepts `size == 0`, which means "to end of file." A single-item
/// AVIF from a stream encoder can use this form for `mdat`.
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

/// Gets a box payload by type at one level.
fn child<'a>(payload: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(payload).find(|(ty, _)| *ty == want).map(|(_, body)| body)
}

/// `ftyp` names AVIF as the major brand or a compatible one. `avis` is the
/// image-sequence brand. It still carries a `meta` box for its primary item, so
/// the parser sizes it the same way.
fn is_avif(bytes: &[u8]) -> bool {
    let Some(ftyp) = boxes(bytes).next().filter(|(ty, _)| *ty == b"ftyp").map(|(_, b)| b) else {
        return false;
    };
    // Major brand, then the version word, then the compatible list - all
    // four-byte tags, so one chunked scan covers every slot.
    let tags = ftyp.as_chunks::<4>().0;
    tags.iter().enumerate().any(|(i, tag)| i != 1 && (tag == b"avif" || tag == b"avis"))
}

/// The size lives in an `ispe` **item property**, not in a header. The parser
/// therefore walks the boxes. `meta` holds `pitm` (the picture's item),
/// `iprp/ipco` (the property list), and `ipma` (the properties for each item).
///
/// This association walk prevents a wrong size. A real AVIF can have an alpha
/// auxiliary item and a thumbnail. Each can have its own `ispe`. The first
/// property can describe a thumbnail instead of the picture. If `pitm` or
/// `ipma` has no value, the parser uses the first `ispe`. That fallback fits the
/// single-item files that a dictionary converter emits.
fn avif_size(bytes: &[u8]) -> Result<Intrinsic, Unreadable> {
    let malformed = Unreadable::Malformed(MediaFormat::Avif);
    let meta = child(bytes, b"meta").ok_or(malformed)?;
    // `meta` is a FullBox. One version byte and three flag bytes come first.
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

    // The property index is 1-based over `ipco` children in source order.
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

/// Gets the `pitm` primary item ID. Version 0 uses sixteen bits. Version 1 uses
/// thirty-two bits.
fn primary_item(body: &[u8]) -> Option<u32> {
    let version = *body.first()?;
    let id = body.get(4..)?;
    if version == 0 {
        Some(be16(id.get(..2)?))
    } else {
        Some(be32(id.get(..4)?))
    }
}

/// Gets the `ipma` property indices associated with one item.
///
/// The parser has two independent width choices. The item ID uses sixteen or
/// thirty-two bits by *version*. The low *flag* bit selects seven or fifteen
/// bits for each property index.
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

/// Gets the root element tag as text.
///
/// XML has no magic number. This function also acts as the sniffer. Bytes with
/// an `<svg` element that starts within [`SVG_WINDOW`] identify an SVG. Other
/// bytes do not. The conversion can replace invalid UTF-8. This behavior is
/// deliberate. The tag is ASCII, and a Latin-1 comment in the preamble must
/// not make the file unreadable.
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

/// Gets one attribute value from a root tag.
///
/// This function matches the whole name. Thus, `stroke-width` never answers a
/// `width` probe. It also matches case exactly because XML parses an SVG
/// referenced by `<img src>` as XML. This is why `viewBox` keeps its capital B.
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
            // XML forbids an unquoted value, but a hand-written file can contain one.
            // Use the run up to whitespace.
            let end = value.find(char::is_whitespace).unwrap_or(value.len());
            return Some(value[..end].to_string());
        }
        rest = &rest[at + name.len()..];
    }
}

/// A CSS length in pixels.
///
/// Returns `None` for a percentage or a font-relative unit. Each value needs a
/// box around the asset, but the asset has no such box, so neither gives an
/// *intrinsic* size. Absolute units use CSS fixed ratios. CSS defines one inch
/// as 96 pixels.
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

/// Gets the ratio from the third and fourth `viewBox` numbers. The root element
/// uses this ratio when it declares no size. This shape is common for a gaiji SVG.
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

/// Applies CSS resolution order for a replaced element. A declared size wins.
/// One declared length plus a ratio gives the other. A ratio alone gives the
/// size. With neither, CSS uses the default object size.
///
/// This last case lets an SVG always get a size. The value is not invented. A
/// browser uses it for a replaced element with no intrinsic size or ratio. The
/// media row therefore exists when the bytes exist, and its size matches the
/// browser result.
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
/// Premultiplied RGBA8 matches tiny-skia's pixel layout. The Linux panel blends
/// a surface without conversion. The Windows adapter swaps two channels once
/// per key while it builds its Direct2D bitmap. This choice costs less because
/// Linux converts each frame and Windows converts each asset.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Why a decode produced no pixels.
///
/// Both variants provide fallback cues, not errors, and both can occur. This
/// build has a rasterizer for every census format, so "no decoder for that
/// format" cannot occur.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undecodable {
    /// A decoder ran and refused the bytes.
    Broken(MediaFormat),
    /// The decoder accepted the bytes, but the surface would exceed
    /// [`MAX_PIXELS`]. This variant is not corruption. The media row is correct,
    /// and the asset is larger than a popup can composite. A diagnostic can state
    /// this cause.
    TooLarge(MediaFormat),
}

impl fmt::Display for Undecodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undecodable::Broken(fmt) => write!(f, "undecodable {fmt}"),
            Undecodable::TooLarge(fmt) => write!(f, "a {fmt} too large to rasterize"),
        }
    }
}

/// Why an asset has no pixels to paint.
///
/// Every variant is a fallback case. None loses a frame. For a broken asset,
/// the popup uses its `alt` text, then a placeholder box. It never leaves a
/// hole in a word. The variants differ only in the diagnostic text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Missing {
    /// No media row. The archive has no asset at that path, or the build could
    /// not read an intrinsic size from the asset.
    NotStored,
    /// The build stored and sized the asset, but cannot paint it.
    Undecodable(Undecodable),
    /// The store did not answer. Carries the message so a bin logs one real
    /// database fault and does not hide it.
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

/// The largest surface a decode returns, in pixels.
///
/// 16 Mpx uses 64 MiB of RGBA8. The largest asset in the census's heaviest
/// image dictionary is 1024x768, which is three orders of magnitude smaller.
/// The bound exists because a container header has a few integer bytes from
/// third-party data. [`MAX_DIM`] rejects an axis over 65 536, but 65 536 x
/// 65 536 still gives 17 GB of pixels.
/// The store's 16 MiB byte cap does not limit *decoded* area for compressed
/// formats. Every variant rejects the area before allocation.
pub const MAX_PIXELS: u32 = 16 << 20;

/// Converts asset bytes to pixels with the format that the media row records.
/// It does not sniff the bytes a second time.
///
/// `at` gives the pixel size for a **vector**. It is the only size input to this
/// half of the module. The layout pass gets the resolved box from the intrinsic
/// size that the build records. [`crate::ui::layout::SceneImage::tint`] quantizes
/// the box into `Tint::Raster`, and the bin passes that pair here. No code reads
/// a decoded size back, so a decoder stays outside the measurement path.
/// `None` rasterizes an SVG at its intrinsic size, and the painter scales the
/// box. A raster format has its own pixels and ignores `at`.
///
/// This function accepts arbitrary bytes, like [`probe`]. Dictionary archives
/// contain third-party data, so every path returns [`Undecodable`] instead of a
/// panic. The `alt`-text ladder then acts on the result.
pub fn decode(
    format: MediaFormat,
    bytes: &[u8],
    at: Option<(u32, u32)>,
) -> Result<Surface, Undecodable> {
    let decoded = match format {
        MediaFormat::Png => decode_png(bytes),
        MediaFormat::Jpeg => decode_jpeg(bytes),
        MediaFormat::Gif => decode_gif(bytes),
        MediaFormat::Svg => decode_svg(bytes, at),
        MediaFormat::Avif => decode_avif(bytes),
    };
    decoded.map_err(|too_big| {
        if too_big { Undecodable::TooLarge(format) } else { Undecodable::Broken(format) }
    })
}

/// The result from one decode: a surface, or `true` when size caused refusal.
///
/// This type uses `bool` instead of a second enum because [`decode`] is its only
/// reader. [`decode`] maps the flag to the format-specific [`Undecodable`]
/// variant. Each decoder below can then use one `?`-chain without another error
/// type.
type Decoded = Result<Surface, bool>;

/// Rejects a surface over [`MAX_PIXELS`] before allocation and returns its pixel
/// count.
fn area(w: u32, h: u32) -> Result<usize, bool> {
    if w == 0 || h == 0 {
        return Err(false);
    }
    match w.checked_mul(h) {
        Some(pixels) if pixels <= MAX_PIXELS => Ok(pixels as usize),
        _ => Err(true),
    }
}

/// One premultiplied pixel channel: `channel * alpha / 255`, with a rounded result.
///
/// This function holds the premultiplied contract in one place. Three callers
/// need the same arithmetic. Different results would create a per-platform hue.
/// [`premultiply`] applies it to a straight sample from a decoder. Each bin's
/// decoded-surface cache applies it to a *tint* color and the alpha of a mask,
/// with its own channel order. The expression is the same in all three
/// paths.
///
/// A rounded result keeps `channel <= alpha`. tiny-skia's pixel type enforces this
/// invariant, and `D2D1_ALPHA_MODE_PREMULTIPLIED` uses it to blend. When
/// `alpha == 255`, `+ 127` returns `channel` exactly. Truncation would leave
/// fully opaque white one below its own alpha.
pub fn premultiplied(channel: u8, alpha: u8) -> u8 {
    ((u32::from(channel) * u32::from(alpha) + 127) / 255) as u8
}

/// Converts straight samples to premultiplied RGBA8. `lanes` gives the number
/// of input channels per pixel.
///
/// One helper serves four decoders because premultiplication is the full
/// [`Surface`] contract. The decoders differ only in channel count: `4` is
/// RGBA, `3` is opaque color, `2` is gray plus alpha, and `1` is gray.
/// The rounded `channel * a / 255` result also keeps `rgb <= a`, as tiny-skia's
/// pixel type requires.
fn premultiply(raw: &[u8], lanes: usize, pixels: usize) -> Result<Vec<u8>, bool> {
    if raw.len() != pixels.checked_mul(lanes).ok_or(true)? {
        return Err(false);
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for px in raw.chunks_exact(lanes) {
        let (r, g, b, a) = match lanes {
            4 => (px[0], px[1], px[2], px[3]),
            3 => (px[0], px[1], px[2], 255),
            2 => (px[0], px[0], px[0], px[1]),
            _ => (px[0], px[0], px[0], 255),
        };
        rgba.extend_from_slice(&[
            premultiplied(r, a),
            premultiplied(g, a),
            premultiplied(b, a),
            a,
        ]);
    }
    Ok(rgba)
}

/// Palette, gray, and 16-bit PNGs all become eight-bit values with alpha.
/// This function widens gray to RGB and applies premultiplication.
fn decode_png(bytes: &[u8]) -> Decoded {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    // The `png` pin bounds its allocations. Give it the area cap before it allocates.
    decoder.set_limits(png::Limits { bytes: MAX_PIXELS as usize * 4 });
    let mut reader = decoder.read_info().map_err(|_| false)?;
    let pixels = area(reader.info().width, reader.info().height)?;
    let mut raw = vec![0u8; reader.output_buffer_size().ok_or(true)?];
    let info = reader.next_frame(&mut raw).map_err(|_| false)?;
    let (w, h) = (info.width, info.height);
    let lanes = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        // `EXPAND` turns a palette into color, so this cannot arrive.
        png::ColorType::Indexed => return Err(false),
    };
    raw.truncate(info.buffer_size());
    Ok(Surface { w, h, rgba: premultiply(&raw, lanes, pixels)? })
}

/// JPEG has no alpha. This function requests four lanes, so [`premultiply`]
/// passes an opaque asset through unchanged.
///
/// Two passes protect the area cap. `decode_headers` gets dimensions from the
/// `SOFn` segment before a progressive JPEG allocates coefficient buffers.
fn decode_jpeg(bytes: &[u8]) -> Decoded {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA));
    decoder.decode_headers().map_err(|_| false)?;
    let (w, h) = decoder.dimensions().ok_or(false)?;
    let (w, h) = (u32::try_from(w).map_err(|_| true)?, u32::try_from(h).map_err(|_| true)?);
    let pixels = area(w, h)?;
    let raw = decoder.decode().map_err(|_| false)?;
    Ok(Surface { w, h, rgba: premultiply(&raw, 4, pixels)? })
}

/// Places the first GIF frame on the logical screen.
///
/// The logical screen is the composite canvas. The first frame can be smaller
/// than this canvas and can have an offset. The media row records the logical
/// screen size (see [`gif_size`]), because that size defines the browser layout.
/// The decoder copies the frame rectangle into that canvas. It does not stretch
/// the frame. The rest of the canvas stays transparent, as for the first frame
/// of an animation.
///
/// The decoder ignores later frames. The spec asks for the first frame, and an
/// animated definition would harm the popup more than a static definition.
fn decode_gif(bytes: &[u8]) -> Decoded {
    /// [`MAX_PIXELS`] in RGBA8 bytes for `gif`'s frame limiter.
    /// A `const` match proves that this value is not zero at compile time.
    /// No unwrap can reach a paint path.
    const FRAME_CAP: std::num::NonZeroU64 =
        match std::num::NonZeroU64::new(MAX_PIXELS as u64 * 4) {
            Some(cap) => cap,
            None => panic!("MAX_PIXELS is not zero"),
        };

    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(FRAME_CAP));
    let mut reader = options.read_info(std::io::Cursor::new(bytes)).map_err(|_| false)?;
    let (w, h) = (u32::from(reader.width()), u32::from(reader.height()));
    let pixels = area(w, h)?;
    let frame = reader.read_next_frame().map_err(|_| false)?.ok_or(false)?;

    let mut rgba = vec![0u8; pixels * 4];
    let stride = w as usize * 4;
    let (left, top) = (usize::from(frame.left), usize::from(frame.top));
    let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
    // Clip instead of a refusal. Old encoders can place a frame rectangle outside
    // the logical screen, and the canvas still defines the line size.
    let span = fw.min((w as usize).saturating_sub(left));
    for row in 0..fh.min((h as usize).saturating_sub(top)) {
        let src = frame.buffer.get(row * fw * 4..).ok_or(false)?;
        let dst = (top + row) * stride + left * 4;
        let premultiplied = premultiply(&src[..span * 4], 4, span)?;
        rgba[dst..dst + span * 4].copy_from_slice(&premultiplied);
    }
    Ok(Surface { w, h, rgba })
}

/// Rasterizes an SVG at `at`, or at its intrinsic size.
///
/// An SVG has no pixels until code selects a size. This decoder is the only
/// decoder whose output size comes from a choice, not from the bytes. `at` is
/// `Tint::Raster`'s pair. [`crate::ui::layout::SceneImage::tint`] has already
/// quantized and clamped it. Thus, a theme change rasterizes a gaiji-sized mask,
/// not a 300x150 default-object illustration.
///
/// The scale uses each axis separately, not one uniform value. The resolved box
/// uses the recorded aspect. One-axis scale would leave empty space inside the
/// box that already defines the line. tiny-skia's pixmap uses premultiplied
/// RGBA8, the [`Surface`] layout, so no conversion is needed.
fn decode_svg(bytes: &[u8], at: Option<(u32, u32)>) -> Decoded {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|_| false)?;
    let size = tree.size();
    let (w, h) = match at {
        Some(wh) => wh,
        // `ceil` preserves a column when the intrinsic size is fractional.
        None => (size.width().ceil() as u32, size.height().ceil() as u32),
    };
    area(w, h)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or(true)?;
    let scale = resvg::tiny_skia::Transform::from_scale(
        w as f32 / size.width(),
        h as f32 / size.height(),
    );
    resvg::render(&tree, scale, &mut pixmap.as_mut());
    Ok(Surface { w, h, rgba: pixmap.take() })
}

/// Decodes AVIF through `avif-rust`. The call handles the container walk, AV1
/// decode, alpha auxiliary item, and container rotate, mirror, and crop.
///
/// This module's `ispe` walk applies the area cap before the decoder. `ispe` is
/// a container property in the first few hundred bytes. A 65 536-square
/// declaration therefore fails before an AV1 decoder allocates a frame. The
/// build uses the same walk to size the asset, so this check uses the value in
/// the media row.
///
/// `avif-rust` returns straight RGBA8. This module applies premultiplication.
fn decode_avif(bytes: &[u8]) -> Decoded {
    let declared = avif_size(bytes).map_err(|_| false)?;
    area(declared.width as u32, declared.height as u32)?;
    let image = avif_rust::image_from_bytes(bytes).map_err(|_| false)?;
    let (w, h) = (
        u32::try_from(image.width).map_err(|_| true)?,
        u32::try_from(image.height).map_err(|_| true)?,
    );
    let pixels = area(w, h)?;
    Ok(Surface { w, h, rgba: premultiply(&image.rgba, 4, pixels)? })
}

// ---- decoded-surface cache policy ----

/// Decoded pixels that a bin's decoded-surface cache retains before eviction,
/// in bytes.
///
/// The census image nodes are gaiji at `height: 1em`. Each decoded asset uses
/// a few kilobytes, so this budget holds thousands. It can hold each distinct
/// glyph touched during a kanji Dictionary hover session. This is a byte
/// budget, not an entry count, because one illustration can equal a thousand
/// gaiji.
///
/// This constant belongs here, not in a bin, because it states a corpus policy,
/// not a pixel-format policy. Both bins hold the same assets at the same sizes.
/// Only channel order differs. A shared budget prevents a second decode on one
/// platform when the other keeps the asset.
pub const SURFACE_BUDGET: usize = 8 << 20;

/// A map bounded by retained value bytes. It evicts entries in insertion order.
///
/// This type holds the admission policy for both bins' decoded-surface caches.
/// It does not touch pixels. The payload is whatever a bin decodes into:
/// tiny-skia's premultiplied RGBA8 pixmap on Linux, or a BGRA byte buffer for
/// `CreateBitmap` on Windows. This type only receives the byte count that the
/// caller gives it. The bins can share this policy while each bin keeps its
/// own paint format (ARCHITECTURE.md#workspace-and-seams). The pixels stay
/// platform-specific.
///
/// Insertion order, not recency, matches the parsed-tree cache in
/// `crate::lookup::sqlite`. A hover covers a window of recent content, so the
/// orders nearly coincide. FIFO needs one push for each miss, while recency
/// needs a touch for each hit.
///
/// The cache uses retained bytes as its bound, not entry count. It holds values
/// from a 12x7 gaiji to a 1024x768 illustration.
pub struct ByteBudget<K, V> {
    /// Stores each answer with its admitted byte count. Eviction needs no second
    /// payload scan, and the total remains equal to the admitted bytes.
    slots: HashMap<K, (V, usize)>,
    order: VecDeque<K>,
    bytes: usize,
    budget: usize,
}

impl<K: Clone + Eq + Hash, V> ByteBudget<K, V> {
    pub fn new(budget: usize) -> ByteBudget<K, V> {
        ByteBudget { slots: HashMap::new(), order: VecDeque::new(), bytes: 0, budget }
    }

    /// Reports whether this key has an answer. A cached refusal counts as a
    /// permanent answer. A new SQLite query each frame would waste work and
    /// produce no pixels.
    pub fn contains_key(&self, key: &K) -> bool {
        self.slots.contains_key(key)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.slots.get(key).map(|(value, _)| value)
    }

    /// Returns retained bytes. This is the sum of every admitted value that
    /// remains in the cache, and it is the amount charged against the budget.
    pub fn footprint(&self) -> usize {
        self.bytes
    }

    /// Admits one answer with `bytes`, then removes entries until the budget holds.
    ///
    /// `len() > 1` keeps one oversized asset. If the method removed the new
    /// entry, the painter would report an asset that the user can see as absent.
    /// The next frame would decode it again. The caller's answer therefore
    /// survives its own admission, and `get` after `admit` always returns it.
    ///
    /// The caller states `bytes` because only the bin knows what its payload
    /// retains. A refusal retains no bytes.
    pub fn admit(&mut self, key: K, value: V, bytes: usize) {
        self.bytes += bytes;
        self.order.push_back(key.clone());
        // Neither cache admits a key twice. Both call `contains_key` first. Keep the
        // total correct if a caller does, and let the stale `order` entry remove nothing.
        if let Some((_, was)) = self.slots.insert(key, (value, bytes)) {
            self.bytes -= was;
        }
        while self.bytes > self.budget && self.order.len() > 1 {
            let Some(evicted) = self.order.pop_front() else { break };
            if let Some((_, was)) = self.slots.remove(&evicted) {
                self.bytes -= was;
            }
        }
    }
}

#[cfg(test)]
mod tests;
