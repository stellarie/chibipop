//! The CPU crop (ADR-0002: shm buffers and CPU cropping everywhere, no
//! GPU plumbing inside a capture backend). Pure pixel arithmetic: a
//! strided compositor buffer in, core's tight top-down BGRA out.
//!
//! `wl_shm` formats name a little-endian *word*, so `Xrgb8888`'s bytes
//! in memory are B, G, R, X - already core's `Frame` layout, which is
//! why the common path is one `copy_from_slice` per row.
//!
//! The other three layouts are not hypothetical. A compositor offers
//! whatever its renderer reads back fastest, and wlroots on GLES2
//! answers `Bgr888` - twenty-four bits, three bytes a pixel, red
//! first. That is what sway hands over on a lot of hardware, so the
//! packed formats are a supported path here, not a courtesy.

use chibipop::geom::{PhysPoint, PhysRect};

/// Byte order of a source pixel, as it sits in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// B, G, R, X - core's own layout.
    Bgrx,
    /// R, G, B, X.
    Rgbx,
    /// B, G, R, packed.
    Bgr,
    /// R, G, B, packed.
    Rgb,
}

impl Order {
    /// Bytes one source pixel occupies.
    pub fn bpp(&self) -> i32 {
        match self {
            Order::Bgrx | Order::Rgbx => 4,
            Order::Bgr | Order::Rgb => 3,
        }
    }

    /// Byte offsets of blue, green and red within a pixel.
    fn channels(&self) -> (usize, usize, usize) {
        match self {
            Order::Bgrx | Order::Bgr => (0, 1, 2),
            Order::Rgbx | Order::Rgb => (2, 1, 0),
        }
    }
}

/// A compositor buffer, exactly as its events described it.
#[derive(Debug, Clone, Copy)]
pub struct Buffer {
    pub w: i32,
    pub h: i32,
    /// Bytes per row, which is not always `w * bpp`.
    pub stride: i32,
    pub order: Order,
    /// The `y_invert` flag: rows arrive bottom-up.
    pub y_invert: bool,
}

/// Copy `src` out of the buffer into `dst` at `dest`.
///
/// `dst` is `dw x dh` BGRA and is left alone wherever the source does
/// not reach - the caller starts it black, so a region hanging off an
/// output reads as blank rather than as garbage. Out-of-range geometry
/// copies nothing instead of panicking: a bad frame must cost one
/// hover, not the process.
pub fn blit(
    bytes: &[u8],
    buf: &Buffer,
    src: PhysRect,
    dest: PhysPoint,
    dst: &mut [u8],
    dw: i32,
    dh: i32,
) {
    let bpp = buf.order.bpp();
    if buf.stride < buf.w * bpp || src.w <= 0 || src.h <= 0 {
        return;
    }
    if src.x < 0 || src.y < 0 || src.x + src.w > buf.w || src.y + src.h > buf.h {
        return;
    }
    if dest.x < 0 || dest.y < 0 || dest.x + src.w > dw || dest.y + src.h > dh {
        return;
    }
    let src_bytes = (src.w * bpp) as usize;
    let dst_bytes = (src.w * 4) as usize;
    let (b, g, r) = buf.order.channels();
    for row in 0..src.h {
        let sy = if buf.y_invert { buf.h - 1 - (src.y + row) } else { src.y + row };
        let so = (sy as usize) * (buf.stride as usize) + (src.x as usize) * bpp as usize;
        let dof = ((dest.y + row) as usize) * (dw as usize) * 4 + (dest.x as usize) * 4;
        let (Some(s), Some(d)) = (
            bytes.get(so..so + src_bytes),
            dst.get_mut(dof..dof + dst_bytes),
        ) else {
            return;
        };
        if buf.order == Order::Bgrx {
            d.copy_from_slice(s);
            continue;
        }
        let (out_px, _) = d.as_chunks_mut::<4>();
        for (px_out, px_in) in out_px.iter_mut().zip(s.chunks_exact(bpp as usize)) {
            px_out[0] = px_in[b];
            px_out[1] = px_in[g];
            px_out[2] = px_in[r];
            // Alpha is junk by contract; opaque is the honest junk.
            px_out[3] = 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w x h` pixels whose blue channel encodes `x` and green `y`.
    fn ramp(w: i32, h: i32, stride: i32) -> Vec<u8> {
        let mut v = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = (y * stride + x * 4) as usize;
                v[o] = x as u8;
                v[o + 1] = y as u8;
                v[o + 2] = 0x40;
                v[o + 3] = 0xff;
            }
        }
        v
    }

    fn buf(w: i32, h: i32, stride: i32) -> Buffer {
        Buffer { w, h, stride, order: Order::Bgrx, y_invert: false }
    }

    fn px(dst: &[u8], dw: i32, x: i32, y: i32) -> [u8; 4] {
        let o = ((y * dw + x) * 4) as usize;
        [dst[o], dst[o + 1], dst[o + 2], dst[o + 3]]
    }

    #[test]
    fn a_subrect_lands_pixel_exact() {
        let bytes = ramp(8, 6, 32);
        let mut dst = vec![0u8; 3 * 2 * 4];
        blit(
            &bytes,
            &buf(8, 6, 32),
            PhysRect { x: 2, y: 3, w: 3, h: 2 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            3,
            2,
        );
        assert_eq!(px(&dst, 3, 0, 0), [2, 3, 0x40, 0xff]);
        assert_eq!(px(&dst, 3, 2, 1), [4, 4, 0x40, 0xff]);
    }

    /// Stride padding must never leak into the output.
    #[test]
    fn padding_between_rows_is_skipped() {
        let mut bytes = ramp(4, 3, 64);
        // Poison the padding.
        for y in 0..3 {
            for b in 16..64 {
                bytes[(y * 64 + b) as usize] = 0xee;
            }
        }
        let mut dst = vec![0u8; 4 * 3 * 4];
        blit(
            &bytes,
            &buf(4, 3, 64),
            PhysRect { x: 0, y: 0, w: 4, h: 3 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            4,
            3,
        );
        assert!(!dst.contains(&0xee), "stride padding leaked into the frame");
    }

    #[test]
    fn a_dest_offset_leaves_the_rest_black() {
        let bytes = ramp(4, 4, 16);
        let mut dst = vec![0u8; 6 * 6 * 4];
        blit(
            &bytes,
            &buf(4, 4, 16),
            PhysRect { x: 0, y: 0, w: 2, h: 2 },
            PhysPoint { x: 4, y: 4 },
            &mut dst,
            6,
            6,
        );
        assert_eq!(px(&dst, 6, 4, 4), [0, 0, 0x40, 0xff]);
        assert_eq!(px(&dst, 6, 0, 0), [0, 0, 0, 0], "untouched pixels stay black");
        assert_eq!(px(&dst, 6, 5, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn y_invert_reads_rows_bottom_up() {
        let bytes = ramp(4, 4, 16);
        let mut dst = vec![0u8; 4 * 2 * 4];
        blit(
            &bytes,
            &Buffer { w: 4, h: 4, stride: 16, order: Order::Bgrx, y_invert: true },
            PhysRect { x: 0, y: 0, w: 4, h: 2 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            4,
            2,
        );
        // Top output row is the buffer's last row.
        assert_eq!(px(&dst, 4, 0, 0)[1], 3);
        assert_eq!(px(&dst, 4, 0, 1)[1], 2);
    }

    #[test]
    fn rgba_order_swaps_red_and_blue() {
        let bytes = vec![0x11, 0x22, 0x33, 0xff];
        let mut dst = vec![0u8; 4];
        blit(
            &bytes,
            &Buffer { w: 1, h: 1, stride: 4, order: Order::Rgbx, y_invert: false },
            PhysRect { x: 0, y: 0, w: 1, h: 1 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            1,
            1,
        );
        assert_eq!(dst, vec![0x33, 0x22, 0x11, 0xff]);
    }

    /// wlroots on GLES2 answers `Bgr888`: three bytes, red first.
    #[test]
    fn packed_twenty_four_bit_pixels_expand_to_bgra() {
        // Two pixels of R,G,B plus one byte of row padding.
        let bytes = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x00];
        let mut dst = vec![0u8; 2 * 4];
        blit(
            &bytes,
            &Buffer { w: 2, h: 1, stride: 7, order: Order::Rgb, y_invert: false },
            PhysRect { x: 0, y: 0, w: 2, h: 1 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            2,
            1,
        );
        assert_eq!(dst, vec![0x33, 0x22, 0x11, 0xff, 0x66, 0x55, 0x44, 0xff]);
    }

    #[test]
    fn packed_bgr_keeps_its_channel_order() {
        let bytes = vec![0x33, 0x22, 0x11];
        let mut dst = vec![0u8; 4];
        blit(
            &bytes,
            &Buffer { w: 1, h: 1, stride: 3, order: Order::Bgr, y_invert: false },
            PhysRect { x: 0, y: 0, w: 1, h: 1 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            1,
            1,
        );
        assert_eq!(dst, vec![0x33, 0x22, 0x11, 0xff]);
    }

    /// A packed pixel offset by x must not read a 4-byte stride.
    #[test]
    fn a_packed_subrect_uses_the_right_pixel_stride() {
        let bytes: Vec<u8> = (0..4u8 * 3).collect();
        let mut dst = vec![0u8; 4];
        blit(
            &bytes,
            &Buffer { w: 4, h: 1, stride: 12, order: Order::Bgr, y_invert: false },
            PhysRect { x: 3, y: 0, w: 1, h: 1 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            1,
            1,
        );
        assert_eq!(dst, vec![9, 10, 11, 0xff]);
    }

    #[test]
    fn every_order_reports_its_own_pixel_size() {
        assert_eq!(Order::Bgrx.bpp(), 4);
        assert_eq!(Order::Rgbx.bpp(), 4);
        assert_eq!(Order::Bgr.bpp(), 3);
        assert_eq!(Order::Rgb.bpp(), 3);
    }

    #[test]
    fn geometry_outside_the_buffer_copies_nothing() {
        let bytes = ramp(4, 4, 16);
        let mut dst = vec![9u8; 4 * 4 * 4];
        for src in [
            PhysRect { x: -1, y: 0, w: 2, h: 2 },
            PhysRect { x: 3, y: 0, w: 2, h: 2 },
            PhysRect { x: 0, y: 0, w: 0, h: 2 },
        ] {
            blit(&bytes, &buf(4, 4, 16), src, PhysPoint { x: 0, y: 0 }, &mut dst, 4, 4);
        }
        assert!(dst.iter().all(|&b| b == 9), "a bad rect must copy nothing");
    }

    #[test]
    fn a_dest_that_does_not_fit_copies_nothing() {
        let bytes = ramp(4, 4, 16);
        let mut dst = vec![9u8; 2 * 2 * 4];
        blit(
            &bytes,
            &buf(4, 4, 16),
            PhysRect { x: 0, y: 0, w: 4, h: 4 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            2,
            2,
        );
        assert!(dst.iter().all(|&b| b == 9));
    }

    #[test]
    fn a_lying_stride_copies_nothing() {
        let bytes = ramp(4, 4, 16);
        let mut dst = vec![9u8; 4 * 4 * 4];
        blit(
            &bytes,
            &buf(4, 4, 8),
            PhysRect { x: 0, y: 0, w: 4, h: 4 },
            PhysPoint { x: 0, y: 0 },
            &mut dst,
            4,
            4,
        );
        assert!(dst.iter().all(|&b| b == 9));
    }
}
