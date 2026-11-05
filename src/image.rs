//! The core PNG and JPEG encoders.
//!
//! Both platform bins send pixels to Anki and disk. Each bin once had its own
//! writer. Windows used WinRT's `BitmapEncoder`, which required its screenshot
//! thread to join a WinRT apartment. Linux wrote stored, uncompressed deflate
//! blocks to avoid a dependency.
//!
//! These details do not belong to a platform. A PNG has one format. Core now
//! owns one pure-Rust encoder. The daemon's capture dumps no longer use
//! raw-size files, and the Windows screenshot thread needs no apartment.
//! Screenshots can also be JPEG and smaller (`shot::encode`).

use anyhow::{Context, Result};

/// Encode `bgra` as a compressed PNG.
/// `bgra` contains `w * h * 4` bytes in top-down BGRA8, the capture `Frame`
/// layout on both platforms.
///
/// Capture paths do not provide useful alpha. `wlr-screencopy` returns the
/// compositor buffer alpha, and `GetDIBits` sets alpha to zero.
/// An RGBA PNG would therefore appear transparent on Windows and contain
/// noise on Linux.
/// The encoder writes color type 2, 8-bit RGB with filtered deflate at the
/// `png` crate default level.
/// The function returns an error when dimensions are not positive or the
/// buffer has fewer than `w * h * 4` bytes.
pub fn encode_bgra_to_png(bgra: &[u8], w: i32, h: i32) -> Result<Vec<u8>> {
    encode_png(bgra, w, h, png::Compression::default())
}

/// Trade wire size for latency when a local OCR process consumes the PNG immediately.
pub fn encode_bgra_to_png_fast(bgra: &[u8], w: i32, h: i32) -> Result<Vec<u8>> {
    encode_png(bgra, w, h, png::Compression::Fast)
}

fn encode_png(bgra: &[u8], w: i32, h: i32, compression: png::Compression) -> Result<Vec<u8>> {
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

    // Convert BGRA -> RGB and discard the alpha byte.
    let mut rgb = Vec::with_capacity(pixels * 3);
    for px in bgra[..need].as_chunks::<4>().0 {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }

    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_compression(compression);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().context("writing the PNG header")?;
    writer.write_image_data(&rgb).context("compressing the PNG")?;
    writer.finish().context("closing the PNG")?;
    Ok(out)
}

/// Encode `bgra` as a baseline JPEG. `quality` is clamped to 1-100.
/// The layout and the errors match [`encode_bgra_to_png`].
pub fn encode_bgra_to_jpeg(bgra: &[u8], w: i32, h: i32, quality: u8) -> Result<Vec<u8>> {
    let dims = u16::try_from(w).ok().zip(u16::try_from(h).ok());
    let Some((w, h)) = dims.filter(|(w, h)| *w > 0 && *h > 0) else {
        anyhow::bail!("a JPEG needs dimensions from 1 to 65535, not {w}x{h}");
    };
    let need = (w as usize) * (h as usize) * 4;
    anyhow::ensure!(
        bgra.len() >= need,
        "{w}x{h} needs {need} bytes of BGRA, got {}",
        bgra.len()
    );
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100))
        .encode(&bgra[..need], w, h, jpeg_encoder::ColorType::Bgra)
        .context("encoding the JPEG")?;
    Ok(out)
}

/// Shrink `bgra` so that its longest side is at most `max`, with a box filter.
/// Returns `None` when the image already fits or `max` is 0.
pub fn shrink_bgra(bgra: &[u8], w: i32, h: i32, max: u32) -> Option<(Vec<u8>, i32, i32)> {
    let (sw, sh) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let longest = sw.max(sh);
    let max = max as usize;
    if max == 0 || longest <= max || bgra.len() < sw * sh * 4 {
        return None;
    }
    let dw = ((sw * max + longest / 2) / longest).max(1);
    let dh = ((sh * max + longest / 2) / longest).max(1);
    let mut out = Vec::with_capacity(dw * dh * 4);
    for dy in 0..dh {
        let (y0, y1) = (dy * sh / dh, ((dy + 1) * sh / dh).max(dy * sh / dh + 1));
        for dx in 0..dw {
            let (x0, x1) = (dx * sw / dw, ((dx + 1) * sw / dw).max(dx * sw / dw + 1));
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &bgra[(y * sw + x0) * 4..(y * sw + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for (acc, &c) in sum.iter_mut().zip(px) {
                        *acc += u32::from(c);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.map(|acc| ((acc + n / 2) / n) as u8));
        }
    }
    Some((out, dw as i32, dh as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode with the same crate. Expect RGB8 rows without interlace.
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

}
