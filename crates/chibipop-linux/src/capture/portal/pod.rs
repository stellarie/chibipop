//! Implements the SPA POD codec, the wire format that PipeWire uses to negotiate formats.
//!
//! **Why this file exists.** Each `spa_pod_builder_*` and `spa_pod_parser_*` entry point in
//! `<spa/pod/builder.h>` is a `static inline` C function.
//! The compiler places these functions in the caller.
//! PipeWire does not export them from `libpipewire-0.3.so`.
//! A client that uses dlopen for PipeWire cannot call them.
//! See [`super::pipewire`] and the manifest comment that explains this choice.
//! Only the format handshake needs these functions.
//! The handshake uses a few hundred bytes of regular serialization.
//! This file writes that data directly. It does not add a build-time
//! `libpipewire-0.3-dev` dependency to the tree.
//!
//! **The format** comes from `<spa/pod/pod.h>`:
//!
//! - Every POD has `u32 size` for the body, not the header, then `u32 type` from
//!   `SPA_TYPE_*`, then `size` body bytes. The next POD starts at the next 8-byte boundary.
//!   The padding does not count in `size`.
//! - An **Object** body has `u32 object_type`, `u32 object_id`, then properties. Each property
//!   has `u32 key`, `u32 flags`, and one POD. This layout aligns each property to 8 bytes.
//!   The iterator needs no extra alignment logic.
//! - A **Choice** body has `u32 choice_type`, `u32 flags`, and one POD header for the
//!   child (`size`, `type`). It then has N raw values of `child.size` bytes with no padding.
//!   The parser recovers N by division. Therefore, final padding must stay outside `size`.
//!
//! All values use native-endian order. This is a local shared-memory-adjacent IPC format,
//! not a network format.
//!
//! The builder API is closed. [`Props`] emits only the value shapes that the ScreenCast handshake uses.
//! The parser is total. It returns `None` for malformed input rather than trust a length from another process.

use crate::capture::crop::Order;

/// These are basic types from `enum spa_type` in `<spa/utils/type.h>`.
/// Include only types that the ScreenCast handshake carries.
/// An unused constant would represent a value that no header checks.
pub const TYPE_ID: u32 = 3;
pub const TYPE_INT: u32 = 4;
pub const TYPE_RECTANGLE: u32 = 10;
pub const TYPE_FRACTION: u32 = 11;
pub const TYPE_OBJECT: u32 = 15;
pub const TYPE_CHOICE: u32 = 19;

/// Object types from `SPA_TYPE_OBJECT_START = 0x40000`.
pub const OBJECT_FORMAT: u32 = 0x40003;
pub const OBJECT_PARAM_BUFFERS: u32 = 0x40004;
pub const OBJECT_PARAM_META: u32 = 0x40005;

/// Values from `enum spa_param_type`.
pub const PARAM_ENUM_FORMAT: u32 = 3;
pub const PARAM_FORMAT: u32 = 4;
pub const PARAM_BUFFERS: u32 = 5;
pub const PARAM_META: u32 = 6;

/// Keys from `enum spa_format`.
pub const FORMAT_MEDIA_TYPE: u32 = 1;
pub const FORMAT_MEDIA_SUBTYPE: u32 = 2;
pub const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
pub const FORMAT_VIDEO_SIZE: u32 = 0x20003;
pub const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;

/// Values from `enum spa_media_type` and `enum spa_media_subtype`.
pub const MEDIA_TYPE_VIDEO: u32 = 2;
pub const MEDIA_SUBTYPE_RAW: u32 = 1;

/// Keys from `enum spa_param_buffers`.
pub const BUFFERS_BUFFERS: u32 = 1;
pub const BUFFERS_BLOCKS: u32 = 2;
pub const BUFFERS_SIZE: u32 = 3;
pub const BUFFERS_STRIDE: u32 = 4;
pub const BUFFERS_ALIGN: u32 = 5;
pub const BUFFERS_DATA_TYPE: u32 = 6;

/// Keys from `enum spa_param_meta`.
pub const META_TYPE: u32 = 1;
pub const META_SIZE: u32 = 2;

/// Values from `enum spa_meta_type`.
pub const META_HEADER: u32 = 1;
/// An array of `struct spa_meta_region` values.
/// A zero dimension ends the array, and an empty array means "this recomposite
/// changed nothing".
/// Core uses this value for `Frame::unchanged` when a push-shaped stream reports no damage.
pub const META_VIDEO_DAMAGE: u32 = 3;
pub const META_CURSOR: u32 = 5;

/// Values from `enum spa_data_type` serve as bit indexes in the `dataType` mask.
pub const DATA_MEM_PTR: u32 = 1;
pub const DATA_MEM_FD: u32 = 2;

/// Values from `enum spa_choice_type` include `SPA_CHOICE_None` (0) and `SPA_CHOICE_Step` (2).
/// [`Pod::fixated`] reads these values directly. It does not use constants.
/// This backend builds neither of these choices.
pub const CHOICE_RANGE: u32 = 1;
pub const CHOICE_ENUM: u32 = 3;
pub const CHOICE_FLAGS: u32 = 4;

/// The backend can read these values from `enum spa_video_format`.
///
/// These names show memory byte order. SPA inherited this convention from GStreamer.
/// Therefore, the `BGRx` format places B, G, R, and x in memory. This order matches core's `Frame` layout.
pub const VIDEO_FORMAT_RGBX: u32 = 7;
pub const VIDEO_FORMAT_BGRX: u32 = 8;
pub const VIDEO_FORMAT_RGBA: u32 = 11;
pub const VIDEO_FORMAT_BGRA: u32 = 12;
pub const VIDEO_FORMAT_RGB: u32 = 15;
pub const VIDEO_FORMAT_BGR: u32 = 16;

/// `sizeof(struct spa_meta_header)` covers two `u32` values and three 8-byte fields that
/// the compiler aligns to 8.
pub const META_HEADER_SIZE: i32 = 32;
/// `sizeof(struct spa_meta_cursor)` covers `id`, `flags`, two `spa_point` values, and
/// `bitmap_offset`.
pub const META_CURSOR_SIZE: i32 = 28;
/// `sizeof(struct spa_meta_bitmap)` covers `format`, `spa_rectangle`, `stride`, and `offset`.
pub const META_BITMAP_SIZE: i32 = 20;

/// The cursor metadata can also hold a `w x h` ARGB bitmap.
///
/// This module never reads the bitmap because the popup draws no cursor.
/// The portal sizes this metadata from the requested size.
/// A bitmap-less request can cause some backends to omit cursor metadata.
pub fn cursor_meta_size(w: i32, h: i32) -> i32 {
    META_CURSOR_SIZE + META_BITMAP_SIZE + w * h * 4
}

/// Returns the `crop::Order` that a SPA video format uses in memory.
/// Returns `None` when core's `Frame` cannot describe the format, such as planar YUV.
///
/// Core ignores alpha bytes.
/// Therefore, the two alpha layouts use their opaque twins.
/// `crop::blit` copies the byte for `Bgrx` or writes `0xff` for `Rgbx`.
/// No later code reads the alpha byte.
pub fn order_of(format: u32) -> Option<Order> {
    match format {
        VIDEO_FORMAT_BGRX | VIDEO_FORMAT_BGRA => Some(Order::Bgrx),
        VIDEO_FORMAT_RGBX | VIDEO_FORMAT_RGBA => Some(Order::Rgbx),
        VIDEO_FORMAT_BGR => Some(Order::Bgr),
        VIDEO_FORMAT_RGB => Some(Order::Rgb),
        _ => None,
    }
}

/// The formats that this backend advertises, in preference order.
///
/// `BGRx` leads because it uses the `crop` memcpy path and common desktop compositors provide it.
/// The packed 24-bit pair follows because a GLES2 readback returns those formats.
pub const OFFERED_FORMATS: [u32; 6] = [
    VIDEO_FORMAT_BGRX,
    VIDEO_FORMAT_BGRA,
    VIDEO_FORMAT_RGBX,
    VIDEO_FORMAT_RGBA,
    VIDEO_FORMAT_BGR,
    VIDEO_FORMAT_RGB,
];

// ---------------------------------------------------------------- build

/// A sequence of object properties in serialized form.
///
/// This builder exposes only the value shapes that the ScreenCast handshake needs.
/// A closed builder rejects an accidental POD before it causes negotiation to fail in another process
/// without a backtrace.
#[derive(Debug, Default)]
pub struct Props {
    buf: Vec<u8>,
}

impl Props {
    pub fn new() -> Props {
        Props { buf: Vec::new() }
    }

    /// Writes `u32 key`, `u32 flags`, and the value POD.
    /// The header uses 8 bytes. Each value POD ends at an 8-byte boundary, so the next property
    /// starts on an 8-byte boundary.
    fn key(&mut self, key: u32) {
        self.buf.extend_from_slice(&key.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes());
    }

    /// Writes one complete POD with its header, body, and alignment bytes to the next 8-byte boundary.
    /// Keep alignment bytes outside `size`.
    fn pod(&mut self, ty: u32, body: &[u8]) {
        let size = u32::try_from(body.len()).unwrap_or(0);
        self.buf.extend_from_slice(&size.to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(body);
        while !self.buf.len().is_multiple_of(8) {
            self.buf.push(0);
        }
    }

    /// Writes a Choice POD with its choice type, flags, child POD header, and adjacent values.
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

    /// Writes `Choice(Enum, Id, ...)`. The first value is preferred, and later values are alternatives.
    pub fn id_enum(&mut self, key: u32, values: &[u32]) -> &mut Props {
        let mut raw = Vec::with_capacity(values.len() * 4);
        for v in values {
            raw.extend_from_slice(&v.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_ENUM, TYPE_ID, 4, &raw);
        self
    }

    /// Writes `Choice(Range, Int, [default, min, max])`.
    pub fn int_range(&mut self, key: u32, default: i32, min: i32, max: i32) -> &mut Props {
        let mut raw = Vec::with_capacity(12);
        for v in [default, min, max] {
            raw.extend_from_slice(&v.to_ne_bytes());
        }
        self.key(key);
        self.choice(CHOICE_RANGE, TYPE_INT, 4, &raw);
        self
    }

    /// Writes `Choice(Flags, Int, [mask])` for the `dataType` mask.
    /// The mask requests shm buffers.
    pub fn int_flags(&mut self, key: u32, mask: i32) -> &mut Props {
        self.key(key);
        self.choice(CHOICE_FLAGS, TYPE_INT, 4, &mask.to_ne_bytes());
        self
    }

    /// Writes `Choice(Range, Rectangle, [default, min, max])`.
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

    /// Writes `Choice(Range, Fraction, [default, min, max])`.
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

/// Returns an Object POD that wraps the properties.
/// The result is ready for `pw_stream_connect` or `pw_stream_update_params`.
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

/// Returns the `SPA_PARAM_EnumFormat` offer that starts negotiation.
/// The offer accepts raw video, [`OFFERED_FORMATS`], sizes from `1 x 1` to `16384 x 16384`,
/// and rates up to 144 Hz.
///
/// The offer omits `SPA_FORMAT_VIDEO_modifier`.
/// This field requests dmabuf. Its absence keeps the stream on shm.
/// The capture backend then uses shm buffers and CPU cropping without GPU plumbing.
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

/// Returns the `SPA_PARAM_Buffers` response for a fixated format.
///
/// `dataType` accepts only `MemPtr | MemFd`.
/// `PW_STREAM_FLAG_MAP_BUFFERS` maps these types.
/// A dmabuf arrives unmapped and has no CPU pointer for cropping.
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

/// Returns a `SPA_PARAM_Meta` request for one metadata block.
pub fn meta(meta_type: u32, size: i32) -> Vec<u8> {
    let mut props = Props::new();
    props.id(META_TYPE, meta_type).int(META_SIZE, size);
    object(OBJECT_PARAM_META, PARAM_META, &props)
}

/// Returns a `SPA_PARAM_Meta` request with a size range.
///
/// Use a range when the server chooses the metadata size, such as a cursor bitmap or damage list.
/// The server rejects an exact size request for such metadata.
pub fn meta_range(meta_type: u32, default: i32, min: i32, max: i32) -> Vec<u8> {
    let mut props = Props::new();
    props.id(META_TYPE, meta_type).int_range(META_SIZE, default, min, max);
    object(OBJECT_PARAM_META, PARAM_META, &props)
}

/// Returns the cursor metadata request for rung 2 of the cursor ladder.
///
/// Use a size range because the portal chooses the bitmap size.
/// This module does not need that size. A fixed request would fail.
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

/// A POD that borrows storage from a byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pod<'a> {
    pub ty: u32,
    pub body: &'a [u8],
}

impl<'a> Pod<'a> {
    /// Reads the POD at the start of `bytes` and returns the offset of the next POD.
    /// Returns `None` when the declared length does not fit.
    /// The bytes come from another process, so the length field is a claim, not a fact.
    pub fn read(bytes: &'a [u8]) -> Option<(Pod<'a>, usize)> {
        let size = u32_at(bytes, 0)? as usize;
        let ty = u32_at(bytes, 4)?;
        let body = bytes.get(8..8 + size)?;
        let advance = (8 + size).next_multiple_of(8);
        Some((Pod { ty, body }, advance))
    }

    /// Parses all of `bytes` as one POD and rejects extra bytes after its padding.
    pub fn parse(bytes: &'a [u8]) -> Option<Pod<'a>> {
        let (pod, advance) = Pod::read(bytes)?;
        (advance >= bytes.len()).then_some(pod)
    }

    /// Returns the first value from a `Choice`. Returns this POD for other types.
    ///
    /// A fixated parameter can arrive as a plain value or as `Choice(None, ...)`.
    /// A server can also return a `Range`, whose default occupies the same slot.
    /// This wrapper handles all forms and avoids different behavior on GNOME and KDE.
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

    /// Returns this POD as an object when its type is `TYPE_OBJECT`.
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

/// The Object POD header and its undecoded property bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object<'a> {
    pub object_type: u32,
    pub id: u32,
    props: &'a [u8],
}

impl<'a> Object<'a> {
    /// Returns properties in wire order.
    /// Stops at the first malformed property rather than guess the next offset.
    pub fn props(&self) -> Properties<'a> {
        Properties { rest: self.props }
    }

    /// Returns a property's value after it unwraps a `Choice` wrapper.
    pub fn value(&self, key: u32) -> Option<Pod<'a>> {
        self.props().find(|(k, _)| *k == key).map(|(_, v)| v.fixated())
    }
}

/// Iterates over an object's properties.
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

/// Describes values from a fixated `SPA_PARAM_Format` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    /// The `enum spa_video_format` id from the payload.
    pub format: u32,
    /// Memory byte order for [`crate::capture::crop`].
    pub order: Order,
    pub width: i32,
    pub height: i32,
    /// `(numerator, denominator)`. A value of `0/1` means "as fast as it damages".
    pub framerate: (u32, u32),
}

impl VideoFormat {
    /// Returns the requested bytes per row when the buffer provides no stride.
    ///
    /// PipeWire reports the real stride in `spa_chunk`.
    /// This value is only the request in `SPA_PARAM_Buffers`.
    pub fn nominal_stride(&self) -> i32 {
        self.width.saturating_mul(self.order.bpp())
    }
}

/// Decodes the `param_changed(SPA_PARAM_Format, ...)` payload.
///
/// Returns `None` for a raw-video format that this module cannot crop.
/// The caller must abandon that negotiation. It must not read a frame incorrectly.
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

    /// The handshake never reads an Int. Therefore, `Pod` has no Int reader.
    /// These tests decode written Int values locally.
    fn int_of(pod: Pod<'_>) -> Option<i32> {
        if pod.ty != TYPE_INT {
            return None;
        }
        let raw: [u8; 4] = pod.body.get(0..4)?.try_into().ok()?;
        Some(i32::from_ne_bytes(raw))
    }

    /// Each POD in a sequence starts on an 8-byte boundary.
    /// Alignment bytes stay outside the size field, which the parser needs.
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

    /// A Choice header uses 16 bytes.
    /// Values have no alignment bytes, so the reader recovers N by division.
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
        // The `fixated` method returns the preferred value.
        assert_eq!(Some(VIDEO_FORMAT_BGRX), raw.fixated().as_id());
    }

    /// A server can narrow to `Choice(None, ...)` instead of sending a bare value.
    /// Both forms must return the same value.
    #[test]
    fn a_none_choice_reads_as_its_single_value() {
        let mut props = Props::new();
        props.key(FORMAT_VIDEO_FORMAT);
        // SPA_CHOICE_None is 0. This module builds no such choice, so it has no constant.
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

    /// A format that this module cannot crop must not become a frame.
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

    /// Length fields come from another process. A false length must fail parsing and never cause
    /// an out-of-bounds read.
    #[test]
    fn a_truncated_pod_is_none_not_a_panic() {
        let mut bytes = enum_format();
        assert!(Pod::parse(&bytes).is_some());
        bytes.truncate(bytes.len() - 8);
        assert_eq!(None, Pod::parse(&bytes));
        assert_eq!(None, Pod::read(&[0u8; 3]));
        assert_eq!(None, parse_video_format(&[]));
    }

    /// The four handshake parameters must be well-formed objects with the expected ID.
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

    /// Request shm only. A dmabuf arrives unmapped, so CPU cropping has no pointer.
    #[test]
    fn the_buffer_param_asks_for_shm_only() {
        let bytes = buffers(4096, 1080);
        let obj = Pod::parse(&bytes).unwrap().as_object().unwrap();
        let mask = int_of(obj.value(BUFFERS_DATA_TYPE).unwrap()).unwrap();
        assert_eq!((1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD), mask);
        assert_eq!(Some(4096 * 1080), int_of(obj.value(BUFFERS_SIZE).unwrap()));
    }

    /// A modifier requests dmabuf. The offer must not name one.
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
