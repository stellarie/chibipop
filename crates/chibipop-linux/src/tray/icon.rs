//! The tray icon: the app's artwork pre-rendered to PNG and decoded to
//! the ARGB32 pixmaps StatusNotifierItem asks for (ADR-0006).
//!
//! Why embedded pixmaps rather than a freedesktop *named* icon: an
//! unpackaged `chibipop` — release tarball, AppImage, plain
//! `cargo build` — has installed nothing under `hicolor/`, so a named
//! icon renders as a blank square on exactly the bare setups this port
//! exists for. A distro package can add the named icon later without
//! touching this module.
//!
//! Why two hand-rendered PNGs rather than rasterising the SVG at
//! startup: the daemon must not link a rasteriser to draw a 24-pixel
//! icon. The assets are produced once, by hand, from the shipped SVG:
//!
//! ```text
//! rsvg-convert -w 24 -h 24 crates/chibipop-windows/assets/chibipop.svg \
//!     -o crates/chibipop-linux/assets/tray-24.png
//! rsvg-convert -w 48 -h 48 crates/chibipop-windows/assets/chibipop.svg \
//!     -o crates/chibipop-linux/assets/tray-48.png
//! ```
//!
//! Hosts pick the closest size and scale; 24 and 48 cover a 1x and a 2x
//! bar without shipping a sprite sheet.

use std::sync::LazyLock;

/// The rendered artwork, smallest first.
const ASSETS: [(&str, &[u8]); 2] = [
    ("tray-24.png", include_bytes!("../../assets/tray-24.png")),
    ("tray-48.png", include_bytes!("../../assets/tray-48.png")),
];

/// The decoded icon set, plus whatever refused to decode.
///
/// A broken asset is a diagnostic, never a failure: SNI treats an empty
/// pixmap list as "no icon", so a bad decode costs the user a blank
/// square and nothing else.
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

/// Decoded once per process; `Tray::icon_pixmap` is called on every
/// property render, and the decode is not worth repeating.
pub fn icons() -> &'static Icons {
    &ICONS
}

/// One 8-bit RGBA PNG to one ARGB32 pixmap.
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

    // RGBA -> ARGB32 in network byte order, which is what `ksni::Icon`
    // documents and what the SNI spec's Pixmap carries. Getting this
    // wrong is invisible in tests and glaring in a bar, hence the
    // golden pixel in this module's tests.
    for pixel in data.chunks_exact_mut(4) {
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

    /// Both committed assets decode, at the sizes their names promise,
    /// with a pixel count the SNI Pixmap layout implies.
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

    /// The byte order is ARGB, not the RGBA the PNG stores. The artwork's
    /// centre pixel is opaque cream, so a missed rotation shows up as a
    /// different leading byte (245, the red channel, instead of 255, the
    /// alpha).
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

    /// A non-PNG blob is a decode problem, not a panic — the whole point
    /// of collecting `problems` instead of unwrapping.
    #[test]
    fn a_corrupt_asset_is_a_diagnostic() {
        let e = decode(b"not a png at all").expect_err("must not decode");
        assert!(e.contains("unreadable PNG header"), "unhelpful message: {e}");
    }
}
