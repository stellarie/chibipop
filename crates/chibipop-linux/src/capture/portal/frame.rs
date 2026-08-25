//! The one frame the portal stream keeps, and the crop out of it.
//!
//! **Why a store at all.** ADR-0002's fallback rung is push-shaped -
//! PipeWire hands whole monitors over whenever the compositor
//! composites - while `RegionCapture` is pull-shaped and must never
//! wait for damage. The join is this: the stream thread parks the
//! newest frame here, and `grab` crops out of whatever is parked.
//!
//! **Why the frame is not copied.** The obvious store is a `Vec<u8>`
//! filled on every `process`. At 4K that is 33 MB memcpy'd per frame
//! by a background daemon that will read a 600x400 box out of it, and
//! ADR-0002 already spends the portal tier's budget on full-monitor
//! *compositor* copies. So the parked frame is the `pw_buffer` itself,
//! still checked out of the stream, and the crop reads straight from
//! its mapped pages. One buffer stays checked out at a time; the
//! previous one goes back the instant a newer one arrives, which is
//! exactly the "hold one, recycle the rest" pattern PipeWire's own
//! consumers use. The raw pointers that implies live behind this
//! module's mutex and nowhere else.
//!
//! **Content change.** [`Latest::content_seq`] increments only for a
//! frame that actually changed something: a buffer carrying
//! `SPA_META_VideoDamage` with no valid region is a recomposite of
//! identical pixels and does not bump it, so a dwell on a static
//! screen can honestly answer `Frame::unchanged`. A stream whose
//! server does not attach that metadata bumps the counter on every
//! frame, which makes `unchanged` permanently false - always correct,
//! merely costing an OCR pass (core's `Frame::unchanged` contract).

use crate::capture::crop;
use chibipop::geom::{PhysPoint, PhysRect};
use std::ffi::c_void;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The negotiated layout of every buffer on the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub w: i32,
    pub h: i32,
    /// Bytes per row, as the arriving `spa_chunk` reported it.
    pub stride: i32,
    pub order: crop::Order,
}

/// The parked frame.
struct Latest {
    /// The `pw_buffer` we hold, or null when we hold none.
    buffer: *mut c_void,
    /// Its mapped pixels. Valid exactly while `buffer` is non-null.
    data: *const u8,
    len: usize,
    shape: Option<Shape>,
    /// Increments for every frame that changed pixels. See the module
    /// doc: this is what `Frame::unchanged` is derived from.
    content_seq: u64,
    /// Whether anything has ever arrived.
    started: bool,
}

/// The stream's newest frame, shared between the PipeWire thread that
/// produces it and the reader thread that crops it.
///
/// SAFETY (the two `unsafe impl`s below): every field of [`Latest`] is
/// reachable only through `inner`, so the raw pointers are never read
/// or written without the mutex. `buffer`/`data` are set together by
/// [`FrameStore::accept`] from the pages PipeWire mapped for us and
/// cleared together by [`FrameStore::release`] before the server may
/// unmap them, so a non-null `data` is always a live mapping of `len`
/// bytes.
pub struct FrameStore {
    inner: Mutex<Latest>,
    /// Signals the very first frame, and only that.
    started: Condvar,
}

// SAFETY: see the type's doc comment.
unsafe impl Send for FrameStore {}
// SAFETY: see the type's doc comment.
unsafe impl Sync for FrameStore {}

impl Default for FrameStore {
    fn default() -> FrameStore {
        FrameStore::new()
    }
}

impl FrameStore {
    pub fn new() -> FrameStore {
        FrameStore {
            inner: Mutex::new(Latest {
                buffer: std::ptr::null_mut(),
                data: std::ptr::null(),
                len: 0,
                shape: None,
                content_seq: 0,
                started: false,
            }),
            started: Condvar::new(),
        }
    }

    /// Park a freshly dequeued buffer.
    ///
    /// Answers with the buffer this one displaced, which the caller
    /// must hand back to the stream - the store never touches
    /// PipeWire itself.
    ///
    /// # Safety
    /// `data` must point at `len` readable bytes that stay mapped
    /// until this buffer comes back through [`FrameStore::release`]
    /// or is displaced by a later `accept`.
    pub unsafe fn accept(
        &self,
        buffer: *mut c_void,
        data: *const u8,
        len: usize,
        shape: Shape,
        changed: bool,
    ) -> *mut c_void {
        let mut latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let displaced = latest.buffer;
        latest.buffer = buffer;
        latest.data = data;
        latest.len = len;
        latest.shape = Some(shape);
        if changed {
            latest.content_seq = latest.content_seq.wrapping_add(1);
        }
        if !latest.started {
            latest.started = true;
            self.started.notify_all();
        }
        displaced
    }

    /// The server is taking `buffer` back. Answers whether it was the
    /// parked one, i.e. whether the caller must *not* queue it.
    pub fn release(&self, buffer: *mut c_void) -> bool {
        let mut latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if latest.buffer != buffer {
            return false;
        }
        latest.buffer = std::ptr::null_mut();
        latest.data = std::ptr::null();
        latest.len = 0;
        true
    }

    /// Forget the parked frame entirely: a renegotiated format or a
    /// switched monitor means the pixels describe somewhere else.
    ///
    /// Answers the buffer to hand back, if one was parked.
    pub fn clear(&self) -> *mut c_void {
        let mut latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let held = latest.buffer;
        latest.buffer = std::ptr::null_mut();
        latest.data = std::ptr::null();
        latest.len = 0;
        latest.shape = None;
        latest.started = false;
        // A switched stream is new content by definition.
        latest.content_seq = latest.content_seq.wrapping_add(1);
        held
    }

    /// Wait for the first frame of this stream, and no longer than
    /// `within`.
    ///
    /// This is the one wait in the backend, and it is not a wait for
    /// *damage* - it is a wait for the stream to start at all, which
    /// happens once. `grab` never reaches it again.
    pub fn wait_for_first(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while !latest.started {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (guard, timeout) = self
                .started
                .wait_timeout(latest, left)
                .unwrap_or_else(|e| e.into_inner());
            latest = guard;
            if timeout.timed_out() && !latest.started {
                return false;
            }
        }
        true
    }

    /// The stream's negotiated shape, once one frame has landed.
    pub fn shape(&self) -> Option<Shape> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).shape
    }

    /// The current content counter, for a caller deciding `unchanged`
    /// without cropping anything.
    pub fn content_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).content_seq
    }

    /// Crop `src` (stream buffer pixels) into `out` at `dest`.
    ///
    /// `out` is `dw x dh` BGRA and is left alone wherever the source
    /// does not reach, exactly as [`crop::blit`] promises - the caller
    /// starts it black so a box hanging off the monitor reads blank.
    /// Answers the content counter the pixels came from, so the caller
    /// can tell a later grab whether anything moved; `None` when no
    /// frame is parked yet.
    pub fn crop_into(
        &self,
        src: PhysRect,
        dest: PhysPoint,
        out: &mut [u8],
        dw: i32,
        dh: i32,
    ) -> Option<u64> {
        let latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let shape = latest.shape?;
        if latest.data.is_null() {
            return None;
        }
        // SAFETY: `data`/`len` describe pages PipeWire mapped for this
        // buffer and cannot be unmapped while we hold the lock - the
        // only paths that unmap go through `release`/`clear`, which
        // take it.
        let bytes = unsafe { std::slice::from_raw_parts(latest.data, latest.len) };
        let buffer = crop::Buffer {
            w: shape.w,
            h: shape.h,
            stride: shape.stride,
            order: shape.order,
            // PipeWire video is top-down; there is no y_invert flag in
            // the stream, unlike wlr-screencopy.
            y_invert: false,
        };
        crop::blit(bytes, &buffer, src, dest, out, dw, dh);
        Some(latest.content_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store only ever compares buffer handles, never dereferences
    /// them, so a distinct address per fake buffer is all a test needs.
    fn tag(n: usize) -> *mut c_void {
        std::ptr::without_provenance_mut(n * std::mem::align_of::<usize>())
    }

    /// A store fed a synthetic "mapping": the pixels live in the test.
    fn store_with(pixels: &[u8], shape: Shape, changed: bool) -> FrameStore {
        let store = FrameStore::new();
        // SAFETY: `pixels` outlives every use in each test below, and
        // nothing releases the buffer.
        unsafe {
            store.accept(tag(1), pixels.as_ptr(), pixels.len(), shape, changed);
        }
        store
    }

    fn bgrx(w: i32, h: i32, stride: i32) -> (Vec<u8>, Shape) {
        let mut buf = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = (y * stride + x * 4) as usize;
                buf[o] = x as u8;
                buf[o + 1] = y as u8;
                buf[o + 2] = 0x40;
                buf[o + 3] = 0xff;
            }
        }
        (buf, Shape { w, h, stride, order: crop::Order::Bgrx })
    }

    #[test]
    fn a_crop_reads_the_requested_box_out_of_the_parked_frame() {
        let (pixels, shape) = bgrx(16, 8, 16 * 4);
        let store = store_with(&pixels, shape, true);
        let mut out = vec![0u8; 4 * 3 * 4];
        let seq = store
            .crop_into(PhysRect { x: 5, y: 2, w: 4, h: 3 }, PhysPoint { x: 0, y: 0 }, &mut out, 4, 3)
            .expect("a parked frame");
        assert_eq!(1, seq);
        // Top-left of the crop is source pixel (5, 2).
        assert_eq!([5, 2, 0x40, 0xff], out[0..4]);
        // Bottom-right is (8, 4).
        assert_eq!([8, 4, 0x40, 0xff], out[out.len() - 4..]);
    }

    /// A stride wider than the row is the normal case, not the odd
    /// one: compositors align rows.
    #[test]
    fn a_padded_stride_does_not_skew_the_crop() {
        let (pixels, shape) = bgrx(16, 8, 16 * 4 + 64);
        let store = store_with(&pixels, shape, true);
        let mut out = vec![0u8; 2 * 2 * 4];
        store
            .crop_into(PhysRect { x: 1, y: 6, w: 2, h: 2 }, PhysPoint { x: 0, y: 0 }, &mut out, 2, 2)
            .unwrap();
        assert_eq!([1, 6, 0x40, 0xff], out[0..4]);
        assert_eq!([2, 7, 0x40, 0xff], out[out.len() - 4..]);
    }

    /// The box may hang off the frame; those pixels stay as the caller
    /// left them (black), because off-monitor has no content.
    #[test]
    fn a_box_reaching_past_the_frame_leaves_the_rest_untouched() {
        let (pixels, shape) = bgrx(8, 8, 32);
        let store = store_with(&pixels, shape, true);
        let mut out = vec![0u8; 4 * 4 * 4];
        // Only the 2x2 at (6,6) exists; place it at the crop's origin.
        store
            .crop_into(PhysRect { x: 6, y: 6, w: 2, h: 2 }, PhysPoint { x: 0, y: 0 }, &mut out, 4, 4)
            .unwrap();
        assert_eq!([6, 6, 0x40, 0xff], out[0..4]);
        assert!(out[(3 * 4 + 3) * 4..].iter().all(|&b| b == 0), "the unreached corner stayed black");
    }

    /// The whole point of the counter: a recomposite with no damage is
    /// the same pixels, and must not look like new content.
    #[test]
    fn an_undamaged_frame_does_not_bump_the_content_counter() {
        let (pixels, shape) = bgrx(4, 4, 16);
        let store = store_with(&pixels, shape, true);
        assert_eq!(1, store.content_seq());
        unsafe {
            store.accept(tag(2), pixels.as_ptr(), pixels.len(), shape, false);
        }
        assert_eq!(1, store.content_seq(), "an undamaged frame is the same content");
        unsafe {
            store.accept(tag(3), pixels.as_ptr(), pixels.len(), shape, true);
        }
        assert_eq!(2, store.content_seq());
    }

    /// Parking a frame hands the previous one back, so exactly one
    /// buffer is ever checked out.
    #[test]
    fn accepting_a_frame_displaces_exactly_the_previous_one() {
        let (pixels, shape) = bgrx(4, 4, 16);
        let store = FrameStore::new();
        unsafe {
            assert!(store
                .accept(tag(7), pixels.as_ptr(), pixels.len(), shape, true)
                .is_null());
            let displaced =
                store.accept(tag(9), pixels.as_ptr(), pixels.len(), shape, true);
            assert_eq!(tag(7), displaced);
        }
        assert!(!store.release(tag(7)), "7 is no longer parked");
        assert!(store.release(tag(9)), "9 is the parked one");
    }

    /// A released buffer's pages may be unmapped, so nothing may crop
    /// out of them afterwards.
    #[test]
    fn a_released_frame_stops_answering_crops() {
        let (pixels, shape) = bgrx(4, 4, 16);
        let store = store_with(&pixels, shape, true);
        let mut out = vec![0u8; 16];
        assert!(store
            .crop_into(PhysRect { x: 0, y: 0, w: 2, h: 2 }, PhysPoint { x: 0, y: 0 }, &mut out, 2, 2)
            .is_some());
        assert!(store.release(tag(1)));
        assert_eq!(
            None,
            store.crop_into(
                PhysRect { x: 0, y: 0, w: 2, h: 2 },
                PhysPoint { x: 0, y: 0 },
                &mut out,
                2,
                2
            )
        );
    }

    /// Switching monitors invalidates the parked pixels and counts as
    /// new content.
    #[test]
    fn clearing_returns_the_held_buffer_and_forgets_the_shape() {
        let (pixels, shape) = bgrx(4, 4, 16);
        let store = store_with(&pixels, shape, true);
        assert_eq!(Some(shape), store.shape());
        assert_eq!(tag(1), store.clear());
        assert_eq!(None, store.shape());
        assert_eq!(2, store.content_seq());
        assert!(store.clear().is_null());
    }

    /// The only wait in the backend has a deadline, and missing it is
    /// an answer rather than a hang.
    #[test]
    fn the_first_frame_wait_gives_up_on_its_deadline() {
        let store = FrameStore::new();
        let started = Instant::now();
        assert!(!store.wait_for_first(Duration::from_millis(30)));
        assert!(started.elapsed() < Duration::from_secs(2));

        let (pixels, shape) = bgrx(4, 4, 16);
        let store = store_with(&pixels, shape, true);
        assert!(store.wait_for_first(Duration::from_millis(0)), "an arrived frame needs no wait");
    }
}
