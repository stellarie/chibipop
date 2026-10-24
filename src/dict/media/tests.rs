//! Tests probe and decode against committed fixtures in
//! `tests/fixtures/media/`. Each census format has one file. Every fixture has
//! a different, non-square size, so bytes from the wrong format cannot pass.

use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Lists every census format with the size from its generator.
const SIZES: [(&str, MediaFormat, f32, f32); 7] = [
    ("one.png", MediaFormat::Png, 12.0, 7.0),
    ("grey.png", MediaFormat::Png, 5.0, 4.0),
    ("unused.png", MediaFormat::Png, 3.0, 3.0),
    ("three.jpg", MediaFormat::Jpeg, 23.0, 11.0),
    ("four.gif", MediaFormat::Gif, 9.0, 5.0),
    ("five.avif", MediaFormat::Avif, 480.0, 120.0),
    ("two.svg", MediaFormat::Svg, 64.0, 32.0),
];

/// Gets one pixel as four premultiplied bytes.
fn px(s: &Surface, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * s.w + x) * 4) as usize;
    s.rgba[at..at + 4].try_into().expect("a surface is w * h * 4 bytes")
}

/// Checks that each channel differs by no more than `tol`, as a lossy format allows.
fn near(got: [u8; 4], want: [u8; 4], tol: i32, what: &str) {
    let off = got.iter().zip(want).map(|(&g, w)| (i32::from(g) - i32::from(w)).abs()).max();
    assert!(off <= Some(tol), "{what}: {got:?} is not within {tol} of {want:?}");
}

/// Every census format reaches pixels. Before this change, `decode` refused four
/// of five formats, and a real gaiji appeared as an outlined box.
///
/// This test checks one sample pixel per format with its generator color. Bytes
/// from the wrong format cannot pass. Each tolerance matches its format. JPEG at
/// quality 80 and AVIF at `-q 40` are lossy. The AVIF fixture comes from
/// `magick gradient:red-blue`, which places red at the top and blue at the
/// bottom. The samples sit two rows inside each edge.
#[test]
fn every_census_format_decodes_to_a_surface() {
    let jpeg = decode(MediaFormat::Jpeg, &fixture("three.jpg"), None).expect("jpeg");
    assert_eq!((23, 11), (jpeg.w, jpeg.h));
    near(px(&jpeg, 11, 5), [0xa0, 0x50, 0x30, 255], 6, "a flat #a05030 jpeg");

    let gif = decode(MediaFormat::Gif, &fixture("four.gif"), None).expect("gif");
    assert_eq!((9, 5), (gif.w, gif.h));
    assert_eq!([255, 0, 0, 255], px(&gif, 1, 1), "the first frame is red");

    // The file declares 64x32 over a 0 0 128 64 viewBox. The rect from
    // viewBox coordinates (8,8)-(120,56) maps to pixels (4,4)-(60,28).
    let svg = decode(MediaFormat::Svg, &fixture("two.svg"), None).expect("svg");
    assert_eq!((64, 32), (svg.w, svg.h));
    assert_eq!([0x30, 0x50, 0xa0, 255], px(&svg, 32, 16), "inside the rect");
    assert_eq!([0, 0, 0, 0], px(&svg, 1, 1), "outside it, and transparent");

    let avif = decode(MediaFormat::Avif, &fixture("five.avif"), None).expect("avif");
    assert_eq!((480, 120), (avif.w, avif.h));
    near(px(&avif, 240, 2), [255, 0, 0, 255], 24, "the red end of the gradient");
    near(px(&avif, 240, 117), [0, 0, 255, 255], 24, "the blue end");
}

/// A decoder handles untrusted dictionary bytes and runs at paint time. A panic
/// can stop the daemon during a hover. The build would otherwise produce one
/// diagnostic line.
///
/// The test checks every prefix of the header region, where a length or a
/// dimension lives, and a stride through the payload. The set is bounded on
/// purpose. A decoder that survives thousands of mutations across five real
/// containers has little chance of a panic from one more mutation. CI runs this
/// test three times per job.
#[test]
fn no_prefix_and_no_corruption_of_any_fixture_panics_in_a_decoder() {
    for (name, format, ..) in SIZES {
        let bytes = fixture(name);
        let mut cuts: Vec<usize> = (0..bytes.len().min(200)).collect();
        let mut next = 200;
        while next < bytes.len() {
            cuts.push(next);
            next = next * 6 / 5 + 1;
        }
        for cut in cuts {
            let _ = decode(format, &bytes[..cut], None);
        }
        for at in (0..bytes.len()).step_by(11) {
            let mut damaged = bytes.clone();
            damaged[at] = 0xff;
            let _ = decode(format, &damaged, None);
            damaged[at] = 0x00;
            let _ = decode(format, &damaged, None);
        }
    }
}

// ---- decoded-surface cache policy ----
