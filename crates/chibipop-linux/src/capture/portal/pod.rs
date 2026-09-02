//! The SPA POD codec: the wire format PipeWire negotiates formats in.
//!
//! **Why this file exists at all.** Every `spa_pod_builder_*` and
//! `spa_pod_parser_*` entry point in `<spa/pod/builder.h>` is a
//! `static inline` C function. They are compiled into the *caller*,
//! never exported from `libpipewire-0.3.so`, so a client that dlopens
//! PipeWire (see [`super::pipewire`], and the manifest comment
//! explaining why we dlopen) cannot borrow them. The format handshake
//! is the only thing that needs them, and the format handshake is a
//! few hundred bytes of very regular serialisation, so it is written
//! out here instead of dragging a build-time `libpipewire-0.3-dev`
//! into the tree.
//!
//! **The format**, from `<spa/pod/pod.h>`:
//!
//! - Every POD is `u32 size` (of the *body*, header excluded), `u32
//!   type` (one of `SPA_TYPE_*`), then `size` body bytes. When PODs
//!   sit next to each other the next one starts at the following
//!   8-byte boundary, and that padding is *not* counted in `size`.
//! - An **Object** body is `u32 object_type`, `u32 object_id`, then a
//!   run of properties. A property is `u32 key`, `u32 flags`, then one
//!   POD - so a property is 8-aligned by construction, and the
//!   iteration never needs to think.
//! - A **Choice** body is `u32 choice_type`, `u32 flags`, one POD
//!   header describing the *child* (`size`, `type`), then N raw values
//!   of `child.size` bytes each with no padding between them. `N` is
//!   recovered by division, which is why the trailing pad must stay
//!   out of `size`.
//!
//! Everything is native-endian: this is a shared-memory-adjacent local
//! IPC format, not a network one.
//!
//! The builder half is deliberately closed: [`Props`] can emit exactly
//! the value shapes the ScreenCast handshake uses, and nothing else.
//! The parser half is deliberately total - it answers `None` on any
//! malformed input rather than trusting a length from another process.

use crate::capture::crop::Order;

/// Basic types, `enum spa_type` in `<spa/utils/type.h>`. Only the ones
/// the ScreenCast handshake actually carries: an unused constant here
/// would be a value nothing has ever checked against a header.
pub const TYPE_ID: u32 = 3;
pub const TYPE_INT: u32 = 4;
pub const TYPE_RECTANGLE: u32 = 10;
pub const TYPE_FRACTION: u32 = 11;
pub const TYPE_OBJECT: u32 = 15;
pub const TYPE_CHOICE: u32 = 19;

/// Object types, `SPA_TYPE_OBJECT_START = 0x40000`.
pub const OBJECT_FORMAT: u32 = 0x40003;
pub const OBJECT_PARAM_BUFFERS: u32 = 0x40004;
pub const OBJECT_PARAM_META: u32 = 0x40005;

/// `enum spa_param_type`.
pub const PARAM_ENUM_FORMAT: u32 = 3;
pub const PARAM_FORMAT: u32 = 4;
pub const PARAM_BUFFERS: u32 = 5;
pub const PARAM_META: u32 = 6;

/// `enum spa_format` keys.
pub const FORMAT_MEDIA_TYPE: u32 = 1;
pub const FORMAT_MEDIA_SUBTYPE: u32 = 2;
pub const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
pub const FORMAT_VIDEO_SIZE: u32 = 0x20003;
pub const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;

/// `enum spa_media_type` / `enum spa_media_subtype`.
pub const MEDIA_TYPE_VIDEO: u32 = 2;
pub const MEDIA_SUBTYPE_RAW: u32 = 1;

/// `enum spa_param_buffers` keys.
pub const BUFFERS_BUFFERS: u32 = 1;
pub const BUFFERS_BLOCKS: u32 = 2;
pub const BUFFERS_SIZE: u32 = 3;
pub const BUFFERS_STRIDE: u32 = 4;
pub const BUFFERS_ALIGN: u32 = 5;
pub const BUFFERS_DATA_TYPE: u32 = 6;

/// `enum spa_param_meta` keys.
pub const META_TYPE: u32 = 1;
pub const META_SIZE: u32 = 2;

/// `enum spa_meta_type`.
pub const META_HEADER: u32 = 1;
/// An array of `struct spa_meta_region`; an entry with a zero
/// dimension ends it, so an empty list means "this recomposite
/// changed nothing" - which is the only honest source of core's
/// `Frame::unchanged` on a push-shaped stream.
pub const META_VIDEO_DAMAGE: u32 = 3;
pub const META_CURSOR: u32 = 5;

/// `enum spa_data_type`, used as a bit index in the `dataType` mask.
pub const DATA_MEM_PTR: u32 = 1;
pub const DATA_MEM_FD: u32 = 2;

/// `enum spa_choice_type`. `SPA_CHOICE_None` (0) and `SPA_CHOICE_Step`
/// (2) are read through by [`Pod::fixated`] rather than named: nothing
/// this backend builds is one.
pub const CHOICE_RANGE: u32 = 1;
pub const CHOICE_ENUM: u32 = 3;
pub const CHOICE_FLAGS: u32 = 4;

/// `enum spa_video_format`, the handful this backend can read.
///
/// The names are memory byte order (the GStreamer convention SPA
/// inherited), so `BGRx` really is B, G, R, x in memory - which is
/// core's `Frame` layout already.
pub const VIDEO_FORMAT_RGBX: u32 = 7;
pub const VIDEO_FORMAT_BGRX: u32 = 8;
pub const VIDEO_FORMAT_RGBA: u32 = 11;
pub const VIDEO_FORMAT_BGRA: u32 = 12;
pub const VIDEO_FORMAT_RGB: u32 = 15;
pub const VIDEO_FORMAT_BGR: u32 = 16;

/// `sizeof(struct spa_meta_header)`: two `u32`, then three 8-byte
/// fields the compiler aligns to 8.
pub const META_HEADER_SIZE: i32 = 32;
/// `sizeof(struct spa_meta_cursor)`: `id`, `flags`, two `spa_point`,
/// `bitmap_offset`.
pub const META_CURSOR_SIZE: i32 = 28;
/// `sizeof(struct spa_meta_bitmap)`: `format`, `spa_rectangle`,
/// `stride`, `offset`.
pub const META_BITMAP_SIZE: i32 = 20;

/// The cursor meta big enough to also carry a `w x h` ARGB bitmap.
///
/// We never read the bitmap - the popup draws no cursor - but the
/// portal sizes its meta from what we ask for, and asking for a
/// bitmap-less cursor meta is how some backends decide not to send
/// cursor metadata at all.
pub fn cursor_meta_size(w: i32, h: i32) -> i32 {
    META_CURSOR_SIZE + META_BITMAP_SIZE + w * h * 4
}

/// The `crop::Order` a SPA video format lays out in memory, or `None`
/// when core's `Frame` cannot describe it (planar YUV and friends).
///
/// Alpha is junk by core's contract, so the two alpha layouts fold
/// into their opaque twins: `crop::blit` either copies the byte
/// through (`Bgrx`) or writes `0xff` (`Rgbx`), and nothing downstream
/// reads it.
pub fn order_of(format: u32) -> Option<Order> {
    match format {
        VIDEO_FORMAT_BGRX | VIDEO_FORMAT_BGRA => Some(Order::Bgrx),
        VIDEO_FORMAT_RGBX | VIDEO_FORMAT_RGBA => Some(Order::Rgbx),
        VIDEO_FORMAT_BGR => Some(Order::Bgr),
        VIDEO_FORMAT_RGB => Some(Order::Rgb),
        _ => None,
    }
}

/// The formats this backend advertises, best first.
///
/// `BGRx` leads because it is `crop`'s memcpy path and what every
/// compositor renderer hands the portal on the common desktop; the
/// packed 24-bit pair trails because a GLES2 readback really does
/// answer with them.
pub const OFFERED_FORMATS: [u32; 6] = [
    VIDEO_FORMAT_BGRX,
    VIDEO_FORMAT_BGRA,
    VIDEO_FORMAT_RGBX,
    VIDEO_FORMAT_RGBA,
    VIDEO_FORMAT_BGR,
    VIDEO_FORMAT_RGB,
];

// ---------------------------------------------------------------- build

/// A run of object properties, being serialised.
///
/// Only the value shapes the ScreenCast handshake needs exist here. A
/// closed builder is the point: a POD written by mistake is a
/// negotiation that fails inside another process, with no backtrace.
#[derive(Debug, Default)]
pub struct Props {
    buf: Vec<u8>,
}

impl Props {
    pub fn new() -> Props {
        Props { buf: Vec::new() }
    }

    /// `u32 key`, `u32 flags`, then the value POD. The header pair is
    /// 8 bytes and every value POD is padded to 8, so the next
    /// property is aligned without any explicit rounding.
    fn key(&mut self, key: u32) {
        self.buf.extend_from_slice(&key.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes());
    }

    /// One complete POD: header, body, and the pad to the next 8-byte
    /// boundary. The pad is deliberately outside `size`.
    fn pod(&mut self, ty: u32, body: &[u8]) {
        let size = u32::try_from(body.len()).unwrap_or(0);
        self.buf.extend_from_slice(&size.to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(body);
        while !self.buf.len().is_multiple_of(8) {
            self.buf.push(0);
        }
    }

    /// A `Choice` POD: choice type, flags, the child's POD header, and
    /// the values back to back with no padding between them.
    fn choice(&mut self, choice: u32, child_ty: u32, child_size: u32, values: &[u8]) {
        let mut body = Vec::with_capacity(16 + values.len());
        body.extend_from_slice(&choice.to_ne_bytes());
        body.extend_from_slice(&0u32.to_ne_bytes());
        body.extend_from_slice(&child_size.to_ne_bytes());
        body.extend_from_slice(&child_ty.to_ne_bytes());
        body.extend_from_slice(values);
        self.pod(TYPE_CHOICE, &body);
    }

    pub fn id(&mut self, key: u32, value: u32) -> &mut Props {
        self.key(key);
        self.pod(TYPE_ID, &value.to_ne_bytes());
        self
    }

    pub fn int(&mut self, key: u32, value: i32) -> &mut Props {
        self.key(key);
        self.pod(TYPE_INT, &value.to_ne_bytes());
        self
    }

    /// `Choice(Enum, Id, ...)`: the first value is the preferred one,
    /// the rest are alternatives.
    pub fn id_enum(&mut self, key: u32, values: &[u32]) -> &mut Props {
        let mut raw = Vec::with_capacity(values.len() * 4);
        for v in values {
            raw.extend_from_slice(&v.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_ENUM, TYPE_ID, 4, &raw);
        self
    }

    /// `Choice(Range, Int, [default, min, max])`.
    pub fn int_range(&mut self, key: u32, default: i32, min: i32, max: i32) -> &mut Props {
        let mut raw = Vec::with_capacity(12);
        for v in [default, min, max] {
            raw.extend_from_slice(&v.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_RANGE, TYPE_INT, 4, &raw);
        self
    }

    /// `Choice(Flags, Int, [mask])`: how `dataType` says "shm, please".
    pub fn int_flags(&mut self, key: u32, mask: i32) -> &mut Props {
        self.key(key);
        self.choice(CHOICE_FLAGS, TYPE_INT, 4, &mask.to_ne_bytes());
        self
    }

    /// `Choice(Range, Rectangle, [default, min, max])`.
    pub fn rect_range(
        &mut self,
        key: u32,
        default: (u32, u32),
        min: (u32, u32),
        max: (u32, u32),
    ) -> &mut Props {
        let mut raw = Vec::with_capacity(24);
        for (w, h) in [default, min, max] {
            raw.extend_from_slice(&w.to_ne_bytes());
            raw.extend_from_slice(&h.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_RANGE, TYPE_RECTANGLE, 8, &raw);
        self
    }

    /// `Choice(Range, Fraction, [default, min, max])`.
    pub fn fraction_range(
        &mut self,
        key: u32,
        default: (u32, u32),
        min: (u32, u32),
        max: (u32, u32),
    ) -> &mut Props {
        let mut raw = Vec::with_capacity(24);
        for (num, denom) in [default, min, max] {
            raw.extend_from_slice(&num.to_ne_bytes());
            raw.extend_from_slice(&denom.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_RANGE, TYPE_FRACTION, 8, &raw);
        self
    }
}

/// Wrap properties in an Object POD, ready to hand to
/// `pw_stream_connect` or `pw_stream_update_params`.
pub fn object(object_type: u32, object_id: u32, props: &Props) -> Vec<u8> {
    let body_len = 8 + props.buf.len();
    let mut out = Vec::with_capacity(8 + body_len);
    out.extend_from_slice(&(body_len as u32).to_ne_bytes());
    out.extend_from_slice(&TYPE_OBJECT.to_ne_bytes());
    out.extend_from_slice(&object_type.to_ne_bytes());
    out.extend_from_slice(&object_id.to_ne_bytes());
    out.extend_from_slice(&props.buf);
    out
}

/// The `SPA_PARAM_EnumFormat` we open the negotiation with: raw video,
/// any of [`OFFERED_FORMATS`], any plausible monitor size, any rate up
/// to 144 Hz.
///
/// `SPA_FORMAT_VIDEO_modifier` is deliberately absent. Naming it is
/// what tells the server we can take dmabuf; leaving it out keeps the
/// stream on shm: shm buffers and CPU cropping everywhere, no GPU
/// plumbing inside a capture backend.
pub fn enum_format() -> Vec<u8> {
    let mut props = Props::new();
    props
        .id(FORMAT_MEDIA_TYPE, MEDIA_TYPE_VIDEO)
        .id(FORMAT_MEDIA_SUBTYPE, MEDIA_SUBTYPE_RAW)
        .id_enum(FORMAT_VIDEO_FORMAT, &OFFERED_FORMATS)
        .rect_range(FORMAT_VIDEO_SIZE, (1920, 1080), (1, 1), (16384, 16384))
        .fraction_range(FORMAT_VIDEO_FRAMERATE, (0, 1), (0, 1), (144, 1));
    object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, &props)
}

/// The `SPA_PARAM_Buffers` answer to a fixated format.
///
/// `dataType` is a flags choice over `MemPtr | MemFd` and nothing
/// else: `PW_STREAM_FLAG_MAP_BUFFERS` maps those two for us, and a
/// dmabuf would arrive unmapped with no CPU pointer to crop from.
pub fn buffers(stride: i32, height: i32) -> Vec<u8> {
    let mut props = Props::new();
    let mask = (1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD);
    props
        .int_range(BUFFERS_BUFFERS, 4, 2, 8)
        .int(BUFFERS_BLOCKS, 1)
        .int(BUFFERS_SIZE, stride.saturating_mul(height))
        .int(BUFFERS_STRIDE, stride)
        .int(BUFFERS_ALIGN, 16)
        .int_flags(BUFFERS_DATA_TYPE, mask);
    object(OBJECT_PARAM_BUFFERS, PARAM_BUFFERS, &props)
}

/// A `SPA_PARAM_Meta` request for one metadata block.
pub fn meta(meta_type: u32, size: i32) -> Vec<u8> {
    let mut props = Props::new();
    props.id(META_TYPE, meta_type).int(META_SIZE, size);
    object(OBJECT_PARAM_META, PARAM_META, &props)
}

/// A `SPA_PARAM_Meta` request whose size is a range.
///
/// Metadata whose size the server chooses (a cursor bitmap, a damage
/// list) is refused when asked for at one exact size.
pub fn meta_range(meta_type: u32, default: i32, min: i32, max: i32) -> Vec<u8> {
    let mut props = Props::new();
    props.id(META_TYPE, meta_type).int_range(META_SIZE, default, min, max);
    object(OBJECT_PARAM_META, PARAM_META, &props)
}

/// The cursor metadata request that makes the cursor ladder's rung 2
/// exist: a size range, because the portal picks a bitmap size we do
/// not care about and a fixed ask would be refused.
pub fn meta_cursor() -> Vec<u8> {
    meta_range(
        META_CURSOR,
        cursor_meta_size(64, 64),
        cursor_meta_size(1, 1),
        cursor_meta_size(256, 256),
    )
}

// --------------------------------------------------------------- parse

fn u32_at(bytes: &[u8], off: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
    Some(u32::from_ne_bytes(raw))
}

/// A POD borrowed out of somebody else's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pod<'a> {
    pub ty: u32,
    pub body: &'a [u8],
}

impl<'a> Pod<'a> {
    /// The POD at the head of `bytes`, and how far the next one
    /// starts. `None` for anything that does not fit - the bytes come
    /// from another process and the length field is its claim, not a
    /// fact.
    pub fn read(bytes: &'a [u8]) -> Option<(Pod<'a>, usize)> {
        let size = u32_at(bytes, 0)? as usize;
        let ty = u32_at(bytes, 4)?;
        let body = bytes.get(8..8 + size)?;
        let advance = (8 + size).next_multiple_of(8);
        Some((Pod { ty, body }, advance))
    }

    /// The whole of `bytes` as one POD, rejecting trailing junk beyond
    /// the pad.
    pub fn parse(bytes: &'a [u8]) -> Option<Pod<'a>> {
        let (pod, advance) = Pod::read(bytes)?;
        (advance >= bytes.len()).then_some(pod)
    }

    /// A `Choice` unwrapped to its first value; anything else answers
    /// itself.
    ///
    /// A fixated param arrives as a plain value, but a server is
    /// entitled to send `Choice(None, ...)` meaning the same thing,
    /// and one that sends a `Range` back has told us its default in
    /// the same slot. Reading through the wrapper costs nothing and
    /// removes a whole class of "worked on GNOME, not on KDE".
    pub fn fixated(&self) -> Pod<'a> {
        if self.ty != TYPE_CHOICE {
            return *self;
        }
        let Some(child_size) = u32_at(self.body, 8) else { return *self };
        let Some(child_ty) = u32_at(self.body, 12) else { return *self };
        match self.body.get(16..16 + child_size as usize) {
            Some(body) => Pod { ty: child_ty, body },
            None => *self,
        }
    }

    pub fn as_id(&self) -> Option<u32> {
        (self.ty == TYPE_ID).then(|| u32_at(self.body, 0))?
    }

    pub fn as_rectangle(&self) -> Option<(u32, u32)> {
        if self.ty != TYPE_RECTANGLE {
            return None;
        }
        Some((u32_at(self.body, 0)?, u32_at(self.body, 4)?))
    }

    pub fn as_fraction(&self) -> Option<(u32, u32)> {
        if self.ty != TYPE_FRACTION {
            return None;
        }
        Some((u32_at(self.body, 0)?, u32_at(self.body, 4)?))
    }

    /// This POD as an object, if it is one.
    pub fn as_object(&self) -> Option<Object<'a>> {
        if self.ty != TYPE_OBJECT {
            return None;
        }
        Some(Object {
            object_type: u32_at(self.body, 0)?,
            id: u32_at(self.body, 4)?,
            props: self.body.get(8..)?,
        })
    }
}

/// An Object POD's header plus its undecoded property run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object<'a> {
    pub object_type: u32,
    pub id: u32,
    props: &'a [u8],
}

impl<'a> Object<'a> {
    /// Every property, in wire order. Stops at the first malformed
    /// one rather than guessing where the next might start.
    pub fn props(&self) -> Properties<'a> {
        Properties { rest: self.props }
    }

    /// One property's value, already read through any Choice wrapper.
    pub fn value(&self, key: u32) -> Option<Pod<'a>> {
        self.props().find(|(k, _)| *k == key).map(|(_, v)| v.fixated())
    }
}

/// Iterator over an object's properties.
#[derive(Debug, Clone, Copy)]
pub struct Properties<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Properties<'a> {
    type Item = (u32, Pod<'a>);

    fn next(&mut self) -> Option<(u32, Pod<'a>)> {
        let key = u32_at(self.rest, 0)?;
        let value_bytes = self.rest.get(8..)?;
        let (pod, advance) = Pod::read(value_bytes)?;
        self.rest = value_bytes.get(advance..).unwrap_or(&[]);
        Some((key, pod))
    }
}

/// What a fixated `SPA_PARAM_Format` told us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    /// The `enum spa_video_format` id, as sent.
    pub format: u32,
    /// Memory byte order for [`crate::capture::crop`].
    pub order: Order,
    pub width: i32,
    pub height: i32,
    /// `(numerator, denominator)`; `0/1` means "as fast as it damages".
    pub framerate: (u32, u32),
}

impl VideoFormat {
    /// Bytes per row when the buffer carries none of its own.
    ///
    /// PipeWire reports the real stride per buffer in `spa_chunk`; this
    /// is only the value we *ask* for in `SPA_PARAM_Buffers`.
    pub fn nominal_stride(&self) -> i32 {
        self.width.saturating_mul(self.order.bpp())
    }
}

/// Decode the `param_changed(SPA_PARAM_Format, ...)` payload.
///
/// `None` means "not a raw-video format we can crop", which is a
/// negotiation to abandon rather than a frame to mis-read.
pub fn parse_video_format(bytes: &[u8]) -> Option<VideoFormat> {
    let object = Pod::parse(bytes)?.as_object()?;
    if object.object_type != OBJECT_FORMAT {
        return None;
    }
    if object.value(FORMAT_MEDIA_TYPE)?.as_id()? != MEDIA_TYPE_VIDEO {
        return None;
    }
    if object.value(FORMAT_MEDIA_SUBTYPE)?.as_id()? != MEDIA_SUBTYPE_RAW {
        return None;
    }
    let format = object.value(FORMAT_VIDEO_FORMAT)?.as_id()?;
    let order = order_of(format)?;
    let (w, h) = object.value(FORMAT_VIDEO_SIZE)?.as_rectangle()?;
    let framerate = object
        .value(FORMAT_VIDEO_FRAMERATE)
        .and_then(|p| p.as_fraction())
        .unwrap_or((0, 1));
    let width = i32::try_from(w).ok()?;
    let height = i32::try_from(h).ok()?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(VideoFormat { format, order, width, height, framerate })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing in the handshake *reads* an Int back, so `Pod` does not
    /// carry a reader for one; the tests that assert what we wrote do
    /// their own decoding.
    fn int_of(pod: Pod<'_>) -> Option<i32> {
        if pod.ty != TYPE_INT {
            return None;
        }
        let raw: [u8; 4] = pod.body.get(0..4)?.try_into().ok()?;
        Some(i32::from_ne_bytes(raw))
    }

    /// Every POD in a run starts 8-aligned, and the pad never counts
    /// towards the size field - the whole parser depends on it.
    #[test]
    fn a_pod_pads_to_eight_without_padding_its_size() {
        let mut props = Props::new();
        props.id(FORMAT_MEDIA_TYPE, MEDIA_TYPE_VIDEO);
        let bytes = object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, &props);
        assert_eq!(0, bytes.len() % 8, "{} bytes", bytes.len());
        let pod = Pod::parse(&bytes).expect("a whole object");
        assert_eq!(TYPE_OBJECT, pod.ty);
        assert_eq!(bytes.len() - 8, pod.body.len());
    }

    #[test]
    fn an_object_round_trips_its_scalar_properties() {
        let mut props = Props::new();
        props.id(FORMAT_MEDIA_TYPE, MEDIA_TYPE_VIDEO).int(BUFFERS_STRIDE, 7680);
        let bytes = object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, &props);
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        assert_eq!(OBJECT_FORMAT, obj.object_type);
        assert_eq!(PARAM_ENUM_FORMAT, obj.id);
        assert_eq!(Some(MEDIA_TYPE_VIDEO), obj.value(FORMAT_MEDIA_TYPE).unwrap().as_id());
        assert_eq!(Some(7680), int_of(obj.value(BUFFERS_STRIDE).unwrap()));
        assert_eq!(None, obj.value(FORMAT_VIDEO_SIZE));
    }

    /// The choice header is 16 bytes and the values follow with no
    /// padding, so the reader can recover N by division.
    #[test]
    fn a_choice_body_is_sixteen_bytes_plus_its_values() {
        let mut props = Props::new();
        props.id_enum(FORMAT_VIDEO_FORMAT, &OFFERED_FORMATS);
        let bytes = object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, &props);
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        let (_, raw) = obj.props().next().unwrap();
        assert_eq!(TYPE_CHOICE, raw.ty);
        assert_eq!(16 + 4 * OFFERED_FORMATS.len(), raw.body.len());
        assert_eq!(CHOICE_ENUM, u32::from_ne_bytes(raw.body[0..4].try_into().unwrap()));
        // ...and reading it fixated yields the preferred value.
        assert_eq!(Some(VIDEO_FORMAT_BGRX), raw.fixated().as_id());
    }

    /// A server may fixate by narrowing to Choice(None, ...) instead
    /// of sending a bare value; both must read the same.
    #[test]
    fn a_none_choice_reads_as_its_single_value() {
        let mut props = Props::new();
        props.key(FORMAT_VIDEO_FORMAT);
        // SPA_CHOICE_None is 0; nothing we build is one, so it has no
        // constant of its own.
        props.choice(0, TYPE_ID, 4, &VIDEO_FORMAT_RGB.to_ne_bytes());
        let bytes = object(OBJECT_FORMAT, PARAM_FORMAT, &props);
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        assert_eq!(Some(VIDEO_FORMAT_RGB), obj.value(FORMAT_VIDEO_FORMAT).unwrap().as_id());
    }

    #[test]
    fn a_fixated_format_decodes_to_a_croppable_shape() {
        let mut props = Props::new();
        props
            .id(FORMAT_MEDIA_TYPE, MEDIA_TYPE_VIDEO)
            .id(FORMAT_MEDIA_SUBTYPE, MEDIA_SUBTYPE_RAW)
            .id(FORMAT_VIDEO_FORMAT, VIDEO_FORMAT_BGRX);
        props.key(FORMAT_VIDEO_SIZE);
        let mut rect = Vec::new();
        rect.extend_from_slice(&2560u32.to_ne_bytes());
        rect.extend_from_slice(&1440u32.to_ne_bytes());
        props.pod(TYPE_RECTANGLE, &rect);
        props.key(FORMAT_VIDEO_FRAMERATE);
        let mut fr = Vec::new();
        fr.extend_from_slice(&60u32.to_ne_bytes());
        fr.extend_from_slice(&1u32.to_ne_bytes());
        props.pod(TYPE_FRACTION, &fr);
        let bytes = object(OBJECT_FORMAT, PARAM_FORMAT, &props);

        let format = parse_video_format(&bytes).expect("a raw BGRx format");
        assert_eq!(VideoFormat {
            format: VIDEO_FORMAT_BGRX,
            order: Order::Bgrx,
            width: 2560,
            height: 1440,
            framerate: (60, 1),
        }, format);
        assert_eq!(2560 * 4, format.nominal_stride());
    }

    /// Anything that is not raw video we can crop is a negotiation to
    /// abandon, not a frame to misread.
    #[test]
    fn a_format_we_cannot_crop_is_refused() {
        let mut props = Props::new();
        props
            .id(FORMAT_MEDIA_TYPE, MEDIA_TYPE_VIDEO)
            .id(FORMAT_MEDIA_SUBTYPE, MEDIA_SUBTYPE_RAW)
            // SPA_VIDEO_FORMAT_NV12: planar, no crop::Order for it.
            .id(FORMAT_VIDEO_FORMAT, 24);
        let bytes = object(OBJECT_FORMAT, PARAM_FORMAT, &props);
        assert_eq!(None, parse_video_format(&bytes));
        assert_eq!(None, order_of(24));
    }

    /// Length fields come from another process; a lying one must fail
    /// the parse, never index out of bounds.
    #[test]
    fn a_truncated_pod_is_none_not_a_panic() {
        let mut bytes = enum_format();
        assert!(Pod::parse(&bytes).is_some());
        bytes.truncate(bytes.len() - 8);
        assert_eq!(None, Pod::parse(&bytes));
        assert_eq!(None, Pod::read(&[0u8; 3]));
        assert_eq!(None, parse_video_format(&[]));
    }

    /// The three params the handshake actually sends must at least be
    /// well-formed objects of the right id.
    #[test]
    fn the_handshake_params_are_well_formed() {
        for (bytes, object_type, id) in [
            (enum_format(), OBJECT_FORMAT, PARAM_ENUM_FORMAT),
            (buffers(7680, 1440), OBJECT_PARAM_BUFFERS, PARAM_BUFFERS),
            (meta(META_HEADER, META_HEADER_SIZE), OBJECT_PARAM_META, PARAM_META),
            (meta_cursor(), OBJECT_PARAM_META, PARAM_META),
        ] {
            let obj = Pod::parse(&bytes).expect("a whole object").as_object().expect("an object");
            assert_eq!(object_type, obj.object_type);
            assert_eq!(id, obj.id);
            assert!(obj.props().count() >= 2, "{object_type:#x} has too few properties");
        }
    }

    /// shm only: a dmabuf arrives unmapped and there is no CPU pointer
    /// to crop from.
    #[test]
    fn the_buffer_param_asks_for_shm_only() {
        let bytes = buffers(4096, 1080);
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        let mask = int_of(obj.value(BUFFERS_DATA_TYPE).unwrap()).unwrap();
        assert_eq!((1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD), mask);
        assert_eq!(Some(4096 * 1080), int_of(obj.value(BUFFERS_SIZE).unwrap()));
    }

    /// Naming a modifier is what invites dmabuf; the offer must not.
    #[test]
    fn the_offer_never_names_a_modifier() {
        let bytes = enum_format();
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        // SPA_FORMAT_VIDEO_modifier
        assert!(obj.props().all(|(k, _)| k != 0x20002));
    }

    #[test]
    fn cursor_meta_leaves_room_for_the_bitmap_the_portal_sizes() {
        assert_eq!(META_CURSOR_SIZE + META_BITMAP_SIZE + 4, cursor_meta_size(1, 1));
        assert!(cursor_meta_size(256, 256) > cursor_meta_size(64, 64));
    }
}
