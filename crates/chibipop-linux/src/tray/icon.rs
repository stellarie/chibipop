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
