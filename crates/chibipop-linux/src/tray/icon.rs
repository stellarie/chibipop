//! This module embeds the tray artwork as PNG and decodes it into the
//! ARGB32 pixmaps that `StatusNotifierItem` requests
//! (ARCHITECTURE.md#platform-integration).
//!
//! Embedded pixmaps support an unpackaged `chibipop`.
//! A release tarball, an AppImage, or plain `cargo build` installs no icon under
//! `hicolor/`, so a freedesktop *named* icon is not available on these systems.
//! A distro package can add the named icon without changes here.
//!
//! This module uses two pre-rendered PNGs instead of SVG rasterization at startup.
//! The daemon must not link a rasterizer for a 24-pixel icon.
//! The shipped SVG produces the assets once by hand:
//!
//! ```text
//! rsvg-convert -w 24 -h 24 crates/chibipop-windows/assets/chibipop.svg \
//!     -o crates/chibipop-linux/assets/tray-24.png
//! rsvg-convert -w 48 -h 48 crates/chibipop-windows/assets/chibipop.svg \
//!     -o crates/chibipop-linux/assets/tray-48.png
//! ```
//!
//! Hosts choose the closest size and scale.
//! The 24 and 48 assets cover 1x and 2x bars without a sprite sheet.

use std::sync::LazyLock;

/// Store the rendered artwork from smallest to largest.
const ASSETS: [(&str, &[u8]); 2] = [
    ("tray-24.png", include_bytes!("../../assets/tray-24.png")),
    ("tray-48.png", include_bytes!("../../assets/tray-48.png")),
];

/// Store decoded icons and diagnostics for assets that fail to decode.
///
/// A broken asset produces a diagnostic, not a failure.
/// SNI treats an empty pixmap list as "no icon".
/// A blank square occurs only if all assets fail to decode.
pub struct Icons {
    pub pixmaps: Vec<ksni::Icon>,
    pub problems: Vec<String>,
}

static ICONS: LazyLock<Icons> = LazyLock::new(|| {
    let mut pixmaps = Vec::with_capacity(ASSETS.len());
    let mut problems = Vec::new();
    for (name, bytes) in ASSETS {
        match decode(bytes) {
            Ok(icon) => pixmaps.push(icon),
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    Icons { pixmaps, problems }
});

/// Decode the icon set once per process.
/// `Tray::icon_pixmap` runs for each property render.
/// A repeated decode would waste work.
pub fn icons() -> &'static Icons {
    &ICONS
}

/// Decode one 8-bit RGBA PNG into one ARGB32 pixmap.
fn decode(bytes: &[u8]) -> Result<ksni::Icon, String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .map_err(|e| format!("unreadable PNG header: {e}"))?;
    let size = reader.output_buffer_size().ok_or("implausible dimensions")?;
    let mut data = vec![0u8; size];
    let info = reader.next_frame(&mut data).map_err(|e| format!("undecodable PNG: {e}"))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!("expected 8-bit RGBA, got {:?}/{:?}", info.color_type, info.bit_depth));
    }
    data.truncate(info.buffer_size());

    // Convert RGBA to ARGB32 in network byte order.
    // `ksni::Icon` documents this order, and the SNI `Pixmap` carries it.
    // A wrong byte order shows a clear error in a bar.
    // The ARGB golden test catches a wrong byte order.
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1);
    }

    Ok(ksni::Icon {
        width: i32::try_from(info.width).map_err(|_| "width out of range".to_string())?,
        height: i32::try_from(info.height).map_err(|_| "height out of range".to_string())?,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode both committed assets at the sizes in their names.
    /// Check the pixel count that the SNI `Pixmap` layout requires.
    #[test]
    fn both_assets_decode_at_their_named_sizes() {
        let icons = icons();
        assert!(icons.problems.is_empty(), "unexpected decode problems: {:?}", icons.problems);
        let sizes: Vec<(i32, i32)> = icons.pixmaps.iter().map(|i| (i.width, i.height)).collect();
        assert_eq!(vec![(24, 24), (48, 48)], sizes, "smallest first, as hosts expect");
        for icon in &icons.pixmaps {
            let expected = (icon.width * icon.height * 4) as usize;
            assert_eq!(expected, icon.data.len(), "4 bytes per pixel, no padding");
        }
    }

    /// The byte order is ARGB, not the RGBA order in the PNG.
    /// The artwork center pixel is opaque cream.
    /// A missed rotation then puts 245, the red channel, first
    /// instead of 255, the alpha.
    #[test]
    fn pixels_are_argb_not_rgba() {
        for icon in &icons().pixmaps {
            let w = icon.width as usize;
            let centre = ((icon.height as usize / 2) * w + w / 2) * 4;
            assert_eq!(
                [255, 245, 240, 225],
                icon.data[centre..centre + 4],
                "centre pixel of the {}px icon must be A,R,G,B",
                icon.width
            );
        }
    }

    /// Treat a non-PNG blob as a decode problem, not a panic.
    /// The `problems` list exists so the code does not unwrap.
    #[test]
    fn a_corrupt_asset_is_a_diagnostic() {
        let e = decode(b"not a png at all").expect_err("must not decode");
        assert!(e.contains("unreadable PNG header"), "unhelpful message: {e}");
    }
}
