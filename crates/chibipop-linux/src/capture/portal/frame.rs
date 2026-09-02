//! This module stores the newest frame from the portal stream and crops
//! requested regions from that frame.
//!
//! **Why a store exists.** The fallback rung receives whole monitors from
//! PipeWire whenever the compositor creates a frame. `RegionCapture` requests
//! a region and must never wait for damage. The stream thread stores the
//! newest frame here, and `grab` crops the requested region from it.
//!
//! **Why the frame is not copied.** A `Vec<u8>` store would copy 33 MB for
//! each 4K frame. The daemon would then read only a 600x400 region. The portal
//! tier already copies each full monitor from the compositor. The store
//! retains the `pw_buffer` from the stream and crops directly from its mapped
//! pages. It retains one buffer at a time. When a newer buffer arrives, it
//! returns the previous buffer at once. PipeWire consumers use this
//! "hold one, recycle the rest" pattern. The module keeps the raw pointers
//! behind its mutex.
//!
//! **Content change.** [`Latest::content_seq`] increments only when pixels
//! change. A buffer with `SPA_META_VideoDamage` but no valid region repeats
//! identical pixels, so a dwell on a static screen can return
//! `Frame::unchanged`. If the server sends no damage metadata, the counter
//! increments for every frame. `unchanged` then stays false, which remains
//! correct but causes another OCR pass (core's `Frame::unchanged` contract).

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
    /// Bytes per row, as `spa_chunk` reports them.
    pub stride: i32,
    pub order: crop::Order,
}

/// The parked frame.
struct Latest {
    /// The `pw_buffer` we hold, or null when we hold none.
    buffer: *mut c_void,
    /// Mapped pixels. They are valid only while `buffer` is non-null.
    data: *const u8,
    len: usize,
    shape: Option<Shape>,
    /// Increments for each frame with changed pixels. The module derives
    /// `Frame::unchanged` from this value.
    content_seq: u64,
    /// Whether a frame has ever arrived.
    started: bool,
}

/// The newest frame from the stream. The PipeWire thread writes it, and the
/// reader thread crops it.
///
/// SAFETY (the two `unsafe impl`s below): every [`Latest`] field is reachable
/// only through `inner`. The mutex protects every read and write of the raw
/// pointers. [`FrameStore::accept`] sets `buffer` and `data` from the pages
/// that PipeWire mapped. [`FrameStore::release`] clears both before the server
/// can unmap them. A non-null `data` therefore points to `len` live bytes.
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

    /// Stores a freshly dequeued buffer.
    ///
    /// Returns the buffer that this one displaced. The caller must return that
    /// buffer to the stream. The store does not access PipeWire.
    ///
    /// # Safety
    /// `data` must point to `len` readable bytes. The bytes must stay mapped
    /// until this buffer reaches [`FrameStore::release`] or a later `accept`
    /// displaces it.
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

    /// Releases `buffer` after the server reclaims it. Returns `true` when it
    /// was the parked buffer, so the caller must not queue it.
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

    /// Drops the parked frame. A renegotiated format or monitor change means
    /// that the pixels describe another location.
    ///
    /// Returns the buffer that the caller must return, when one was parked.
    pub fn clear(&self) -> *mut c_void {
        let mut latest = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let held = latest.buffer;
        latest.buffer = std::ptr::null_mut();
        latest.data = std::ptr::null();
        latest.len = 0;
        latest.shape = None;
        latest.started = false;
        // A monitor change always marks new content.
        latest.content_seq = latest.content_seq.wrapping_add(1);
        held
    }

    /// Waits for the first frame of this stream for at most `within`.
    ///
    /// This is the only backend wait. It does not wait for damage. It waits for
    /// the stream to start, which happens once. `grab` does not call it again.
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

    /// Returns the current content counter. The caller can compare it with a
    /// previous value without a frame crop.
    pub fn content_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).content_seq
    }

    /// Crops `src` from stream buffer pixels into `out` at `dest`.
    ///
    /// `out` is `dw x dh` BGRA. The function does not change pixels outside
    /// the source, as [`crop::blit`] specifies. The caller fills `out` with
    /// black, so a box beyond the monitor leaves those pixels black.
    /// Returns the content counter for the source pixels. The caller can
    /// detect changes on a later grab. Returns `None` when no frame is parked.
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
        // SAFETY: `data` and `len` describe pages that PipeWire mapped for this
        // buffer. The lock keeps these pages mapped while this code reads them. Only
        // `release` and `clear` can unmap these pages, and both take the lock.
        let bytes = unsafe { std::slice::from_raw_parts(latest.data, latest.len) };
        let buffer = crop::Buffer {
            w: shape.w,
            h: shape.h,
            stride: shape.stride,
            order: shape.order,
            // PipeWire video uses top-down rows. The stream has no `y_invert` flag,
            // unlike wlr-screencopy.
            y_invert: false,
        };
        crop::blit(bytes, &buffer, src, dest, out, dw, dh);
        Some(latest.content_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store compares buffer handles and never dereferences them. Each test
    /// therefore needs only a distinct address for each fake buffer.
    fn tag(n: usize) -> *mut c_void {
        std::ptr::without_provenance_mut(n * std::mem::align_of::<usize>())
    }

    /// Creates a store with test pixels as the buffer contents.
    fn store_with(pixels: &[u8], shape: Shape, changed: bool) -> FrameStore {
        let store = FrameStore::new();
        // SAFETY: Each test keeps `pixels` alive for every use, and no test releases
        // the buffer.
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

    /// A stride wider than one row is normal because compositors align rows.
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

    /// A box can extend beyond the frame. The caller keeps those pixels black
    /// because the monitor has no content there.
    #[test]
    fn a_box_reaching_past_the_frame_leaves_the_rest_untouched() {
        let (pixels, shape) = bgrx(8, 8, 32);
        let store = store_with(&pixels, shape, true);
        let mut out = vec![0u8; 4 * 4 * 4];
        // The frame contains only the 2x2 box at (6,6). The crop places it at the origin.
        store
            .crop_into(PhysRect { x: 6, y: 6, w: 2, h: 2 }, PhysPoint { x: 0, y: 0 }, &mut out, 4, 4)
            .unwrap();
        assert_eq!([6, 6, 0x40, 0xff], out[0..4]);
        assert!(out[(3 * 4 + 3) * 4..].iter().all(|&b| b == 0), "the unreached corner stayed black");
    }

    /// A recomposite without damage repeats the same pixels. The counter must not
    /// report new content.
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

    /// A new frame returns the previous buffer. The store therefore retains only
    /// one buffer at a time.
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

    /// The server can unmap a released buffer. The store must not crop from its
    /// pages after release.
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

    /// A monitor change invalidates the parked pixels and marks new content.
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

    /// The backend has a deadline for its only wait. A missed deadline returns
    /// an answer instead of an indefinite wait.
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
