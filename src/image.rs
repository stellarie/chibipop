//! The one PNG encoder.
//!
//! Both bins hand pixels to Anki and to disk, and both used to carry
//! their own writer: the Windows bin drove WinRT's `BitmapEncoder`
//! (which is why its screenshot thread had to join a WinRT apartment),
//! and the Linux bin hand-rolled stored - uncompressed - deflate blocks
//! to avoid a dependency. Neither is a platform fact: a PNG is a PNG.
//! One pure-Rust encoder in core replaces both, the daemon's capture
//! dumps stop being raw-size files, and the Windows screenshot thread
//! needs no apartment.

use anyhow::{Context, Result};

/// Bytes of a compressed PNG holding `bgra`, which is `w * h * 4` bytes
/// of top-down BGRA8 - the capture `Frame` layout on both platforms.
///
/// Alpha is dropped rather than carried: every capture path in this tree
/// declares it junk (wlr-screencopy hands back whatever the compositor's
/// buffer held, `GetDIBits` leaves it zero), so an RGBA PNG would encode
/// a fully transparent picture on Windows and noise on Linux. Colour
/// type 2, 8-bit, filtered and deflated by the `png` crate at its
/// default level.
pub fn encode_bgra_to_png(bgra: &[u8], w: i32, h: i32) -> Result<Vec<u8>> {
    let dims = u32::try_from(w).ok().zip(u32::try_from(h).ok());
    let Some((w, h)) = dims.filter(|(w, h)| *w > 0 && *h > 0) else {
        anyhow::bail!("a PNG needs positive dimensions, not {w}x{h}");
    };
    let pixels = (w as usize) * (h as usize);
    let need = pixels * 4;
    anyhow::ensure!(
        bgra.len() >= need,
        "{w}x{h} needs {need} bytes of BGRA, got {}",
        bgra.len()
    );

    // BGRA -> RGB, dropping the junk byte.
    let mut rgb = Vec::with_capacity(pixels * 3);
    for px in bgra[..need].as_chunks::<4>().0 {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().context("writing the PNG header")?;
    writer.write_image_data(&rgb).context("compressing the PNG")?;
    writer.finish().context("closing the PNG")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode with the same crate's reader: RGB8 rows, no interlace.
    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(png::ColorType::Rgb, info.color_type);
        assert_eq!(png::BitDepth::Eight, info.bit_depth);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn a_single_red_pixel_survives_the_bgra_to_rgb_swap() {
        // BGRA: blue 0, green 0, red 255, alpha junk.
        let png = encode_bgra_to_png(&[0, 0, 0xff, 0x00], 1, 1).unwrap();
        assert_eq!((1, 1, vec![0xff, 0, 0]), decode(&png));
    }

    #[test]
    fn every_pixel_of_a_multi_row_image_survives_a_round_trip() {
        let (w, h) = (200i32, 120i32);
        let bgra: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 251) as u8).collect();
        let png = encode_bgra_to_png(&bgra, w, h).unwrap();
        let (dw, dh, rgb) = decode(&png);
        assert_eq!((w as u32, h as u32), (dw, dh));
        assert_eq!((w * h * 3) as usize, rgb.len());
        for (src, dst) in bgra.as_chunks::<4>().0.iter().zip(rgb.as_chunks::<3>().0) {
            assert_eq!(&[src[2], src[1], src[0]], dst, "BGR -> RGB");
        }
    }

    #[test]
    fn the_encoded_stream_is_compressed_not_stored() {
        // 320 KB of flat pixels: a stored-deflate writer emits them all,
        // any real compressor flattens them to nothing.
        let (w, h) = (400i32, 200i32);
        let png = encode_bgra_to_png(&vec![0x40u8; (w * h * 4) as usize], w, h).unwrap();
        assert!(
            png.len() < (w * h * 3) as usize / 100,
            "a uniform {w}x{h} picture encoded to {} bytes",
            png.len()
        );
    }

    #[test]
    fn a_buffer_too_short_for_the_dimensions_is_refused() {
        let e = encode_bgra_to_png(&[0, 0, 0, 0], 4, 4).unwrap_err();
        assert!(e.to_string().contains("needs 64 bytes"), "{e:#}");
    }

    #[test]
    fn a_buffer_longer_than_the_dimensions_encodes_only_the_picture() {
        let bgra = vec![0u8; 4 * 4 * 4 + 999];
        let png = encode_bgra_to_png(&bgra, 4, 4).unwrap();
        assert_eq!((4, 4, vec![0u8; 4 * 4 * 3]), decode(&png));
    }

    #[test]
    fn an_empty_or_negative_region_is_refused() {
        assert!(encode_bgra_to_png(&[], 0, 4).is_err());
        assert!(encode_bgra_to_png(&[], 4, 0).is_err());
        assert!(encode_bgra_to_png(&[], -4, 4).is_err());
    }
}
