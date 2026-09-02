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

#[test]
fn the_intrinsic_size_comes_out_of_the_container_header_for_every_census_format() {
    for (name, format, w, h) in SIZES {
        let got = probe(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(format, got.format, "{name}");
        assert_eq!((w, h), (got.width, got.height), "{name}");
    }
}

#[test]
fn the_recorded_aspect_ratio_is_what_sizes_an_image_that_declares_only_a_height() {
    // The `height: 1em` shape used by 字通 and 三省堂 needs its width from the
    // ratio. The ratio must remain a column, not a derivation.
    let avif = probe(&fixture("five.avif")).unwrap();
    assert_eq!(4.0, avif.aspect, "480x120");
    let png = probe(&fixture("one.png")).unwrap();
    assert_eq!(12.0 / 7.0, png.aspect);
}

#[test]
fn an_animated_gif_is_sized_by_its_logical_screen_and_not_by_its_first_frame() {
    // The fixture has two 4x3 frames inside a 9x5 canvas. A browser lays out the
    // canvas, and this build composites its first frame into it. Therefore, 9x5
    // is the intrinsic size, not 4x3.
    let got = probe(&fixture("four.gif")).unwrap();
    assert_eq!((9.0, 5.0), (got.width, got.height));
}

#[test]
fn an_svg_with_no_declared_size_takes_its_view_box() {
    let got = probe(&fixture("ratio.svg")).unwrap();
    assert_eq!((MediaFormat::Svg, 100.0, 40.0), (got.format, got.width, got.height));
}

/// One declared length plus a ratio gives the other length. This is CSS
/// resolution order for a replaced element.
#[test]
fn an_svg_sizes_the_missing_length_from_its_ratio() {
    let by_width = br#"<svg width="50" viewBox="0 0 10 4"></svg>"#;
    let got = probe(by_width).unwrap();
    assert_eq!((50.0, 20.0), (got.width, got.height));

    let by_height = br#"<svg height="8" viewBox="0 0 10 4"></svg>"#;
    let got = probe(by_height).unwrap();
    assert_eq!((20.0, 8.0), (got.width, got.height));
}

#[test]
fn an_svg_with_neither_a_size_nor_a_ratio_takes_the_css_default_object_size() {
    // 300x150 is the CSS default object size. A browser uses it for a replaced
    // element with no intrinsic size or ratio. The row exists, so layout renders
    // the asset instead of its `alt` text.
    let got = probe(br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#).unwrap();
    assert_eq!((300.0, 150.0), (got.width, got.height));
}

#[test]
fn absolute_svg_units_convert_to_pixels_and_relative_ones_name_no_size() {
    let inches = probe(br#"<svg width="1in" height="72pt"></svg>"#).unwrap();
    assert_eq!((96.0, 96.0), (inches.width, inches.height));

    // A percentage needs a box around the asset, but the asset has no such box.
    // The value is therefore *absent*, and the `viewBox` supplies intrinsic size.
    let relative = br#"<svg width="100%" height="50%" viewBox="0 0 20 10"></svg>"#;
    let got = probe(relative).unwrap();
    assert_eq!((20.0, 10.0), (got.width, got.height));
}

#[test]
fn an_attribute_probe_matches_whole_names_only() {
    // `stroke-width` must not answer a `width` probe. Otherwise, a stroked gaiji
    // would use its pen width as its layout width.
    let tag = br#"<svg stroke-width="9" viewBox="0 0 30 15"></svg>"#;
    let got = probe(tag).unwrap();
    assert_eq!((30.0, 15.0), (got.width, got.height));
}

/// Builds an ISOBMFF box from a four-byte length, a type, and a payload.
fn ibox(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(ty);
    out.extend_from_slice(body);
    out
}

fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut body = vec![0u8; 4];
    body.extend_from_slice(&w.to_be_bytes());
    body.extend_from_slice(&h.to_be_bytes());
    ibox(b"ispe", &body)
}

/// Builds an AVIF whose `ipco` lists a decoy `ispe` before the primary item.
///
/// A real AVIF can have an alpha auxiliary item and a thumbnail. Each can have
/// its own `ispe`. The first property can describe the wrong item. `pitm` plus
/// `ipma` identifies the picture property.
fn avif_with_two_extents(associate: bool) -> Vec<u8> {
    let mut ipco = ispe(7, 3);
    ipco.extend(ispe(40, 25));
    let iprp = ibox(b"iprp", &ibox(b"ipco", &ipco));

    let mut meta = vec![0u8; 4];
    if associate {
        // `pitm`: version 0, flags 0, primary item 2.
        meta.extend(ibox(b"pitm", &[0, 0, 0, 0, 0, 2]));
        // `ipma`: version 0, flags 0, two entries. Each entry has one narrow
        // essential property index. Item 1 uses property 1. Item 2 uses property 2.
        let mut ipma = vec![0u8; 4];
        ipma.extend(2u32.to_be_bytes());
        ipma.extend([0, 1, 1, 0x81]);
        ipma.extend([0, 2, 1, 0x82]);
        meta.extend(ibox(b"ipma", &ipma));
    }
    meta.extend(iprp);

    let mut out = ibox(b"ftyp", b"avifmif1");
    out.extend(ibox(b"meta", &meta));
    out.extend(ibox(b"mdat", &[0u8; 4]));
    out
}

#[test]
fn an_avif_is_sized_by_its_primary_items_extents_and_not_by_the_first_it_finds() {
    let got = probe(&avif_with_two_extents(true)).unwrap();
    assert_eq!((MediaFormat::Avif, 40.0, 25.0), (got.format, got.width, got.height));
}

#[test]
fn an_avif_with_no_item_association_falls_back_to_its_first_extents() {
    // This shape matches a single-item dictionary file: one `ispe` and no
    // association. The parser still sizes it instead of a refusal.
    let got = probe(&avif_with_two_extents(false)).unwrap();
    assert_eq!((7.0, 3.0), (got.width, got.height));
}

#[test]
fn a_recognised_container_with_no_readable_size_is_unreadable_and_not_a_panic() {
    // The committed `broken.png` starts with a PNG signature, but no `IHDR` follows.
    // The build skips it, records a diagnostic, and writes no row.
    assert_eq!(Err(Unreadable::Malformed(MediaFormat::Png)), probe(&fixture("broken.png")));
    // A JPEG that reaches its scan data without a frame header.
    assert_eq!(
        Err(Unreadable::Malformed(MediaFormat::Jpeg)),
        probe(b"\xff\xd8\xff\xda\x00\x02")
    );
    // A zero dimension is a size no layout can use.
    let zero = [
        b"\x89PNG\r\n\x1a\n".as_slice(),
        &[0, 0, 0, 13],
        b"IHDR",
        &[0, 0, 0, 0],
        &[0, 0, 0, 8],
    ]
    .concat();
    assert_eq!(Err(Unreadable::Malformed(MediaFormat::Png)), probe(&zero));
}

#[test]
fn bytes_of_no_census_format_are_unrecognised() {
    for bytes in [b"".as_slice(), b"hello", b"PK\x03\x04nonsense", b"<html><body>"] {
        assert_eq!(Err(Unreadable::Unrecognised), probe(bytes), "{bytes:?}");
    }
}

/// The build must survive a truncated or corrupt asset. An archive contains
/// third-party bytes, and a panic in a probe would stop the whole rebuild.
#[test]
fn no_prefix_and_no_single_byte_corruption_of_any_fixture_panics() {
    for (name, ..) in SIZES {
        let bytes = fixture(name);
        for cut in 0..=bytes.len() {
            let _ = probe(&bytes[..cut]);
        }
        for at in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[at] = 0xff;
            let _ = probe(&damaged);
            damaged[at] = 0x00;
            let _ = probe(&damaged);
        }
    }
}

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

#[test]
fn decoding_premultiplies_alpha_into_the_surface() {
    // The fixture has 12x7 pixels of pure blue at half alpha. The premultiplied
    // pixel is (0, 0, 128, 128). A straight value would make every gaiji too
    // bright over a dark panel.
    let png = fixture("one.png");
    let s = decode(MediaFormat::Png, &png, None).expect("a true-colour PNG decodes");
    assert_eq!((12, 7), (s.w, s.h));
    assert_eq!(12 * 7 * 4, s.rgba.len());
    for px in s.rgba.as_chunks::<4>().0 {
        assert_eq!(&[0, 0, 128, 128], px);
    }
}

#[test]
fn grey_and_palette_pngs_widen_to_four_lanes() {
    for (name, w, h) in [("grey.png", 5u32, 4u32), ("unused.png", 3, 3)] {
        let s = decode(MediaFormat::Png, &fixture(name), None)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((w, h), (s.w, s.h), "{name}");
        assert_eq!((w * h * 4) as usize, s.rgba.len(), "{name}");
    }
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

/// The spec asks for the first frame, and the media row records the logical
/// screen. The decoder must composite the frame into that canvas, not stretch it.
#[test]
fn an_animated_gif_decodes_its_first_frame_into_the_logical_screen() {
    // The canvas has two 4x3 frames, with red first and blue second.
    let gif = decode(MediaFormat::Gif, &fixture("four.gif"), None).expect("gif");
    assert_eq!((9, 5), (gif.w, gif.h), "the canvas, not the frame");
    for px in gif.rgba.as_chunks::<4>().0 {
        assert_ne!(&[0, 0, 255, 255], px, "the second frame must not be painted");
    }
    // Outside the frame rectangle, the canvas stays transparent. The first frame
    // composites onto this area.
    assert_eq!([0, 0, 0, 0], px(&gif, 8, 4));
    assert_eq!([255, 0, 0, 255], px(&gif, 3, 2), "the last pixel of the frame");
}

/// `Tint::Raster` exists because a vector has no pixels until code selects a
/// size. The pair that core quantizes from the resolved box becomes the pixmap
/// that the bin receives.
#[test]
fn an_svg_rasterizes_at_the_size_the_scene_asked_for() {
    let bytes = fixture("two.svg");
    let asked = decode(MediaFormat::Svg, &bytes, Some((16, 8))).expect("svg at a size");
    assert_eq!((16, 8), (asked.w, asked.h));
    assert_eq!(16 * 8 * 4, asked.rgba.len());
    // The rect scales with the box and remains inside it.
    assert_eq!([0x30, 0x50, 0xa0, 255], px(&asked, 8, 4));

    let intrinsic = decode(MediaFormat::Svg, &bytes, None).expect("svg at its own size");
    assert_eq!((64, 32), (intrinsic.w, intrinsic.h));

    // A raster asset has its own pixels, so this request does not change it.
    // `at` applies only to a vector.
    let png = decode(MediaFormat::Png, &fixture("one.png"), Some((99, 99))).expect("png");
    assert_eq!((12, 7), (png.w, png.h));
}

/// A vector with no declared size still reaches pixels. Its `viewBox` gives the
/// intrinsic size in the media row. This shape matches a gaiji from a font glyph.
/// All 35 SVG assets in this library use this shape.
#[test]
fn an_svg_sized_only_by_its_view_box_still_reaches_pixels() {
    let s = decode(MediaFormat::Svg, &fixture("ratio.svg"), None).expect("svg");
    assert_eq!((100, 40), (s.w, s.h));
    assert_eq!([0xa0, 0x50, 0x30, 255], px(&s, 50, 20));
}

/// `Surface` requires premultiplied pixels, and tiny-skia requires `rgb <= a`.
/// A surface that breaks this invariant blends brighter than the asset on every
/// frame.
#[test]
fn every_decoded_surface_is_premultiplied() {
    for (name, format, ..) in SIZES {
        let s = decode(format, &fixture(name), None).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((s.w * s.h * 4) as usize, s.rgba.len(), "{name}");
        for p in s.rgba.as_chunks::<4>().0 {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3], "{name}: {p:?}");
        }
    }
}

#[test]
fn a_ruined_png_answers_broken_rather_than_producing_pixels() {
    assert_eq!(
        Err(Undecodable::Broken(MediaFormat::Png)),
        decode(MediaFormat::Png, &fixture("broken.png"), None)
    );
}

/// The test truncates half of each fixture, one format at a time. A definite
/// `Err` activates the `alt`-text ladder. The media row still has the correct
/// intrinsic size, so the line layout does not change.
#[test]
fn a_truncated_asset_of_any_format_answers_undecodable_rather_than_pixels() {
    for (name, format, ..) in SIZES {
        let bytes = fixture(name);
        let half = &bytes[..bytes.len() / 2];
        assert!(decode(format, half, None).is_err(), "half of {name} decoded");
    }
}

/// A declared area over the cap must fail before allocation.
///
/// Both checks matter. The GIF fixture has its logical screen descriptor
/// overwritten, with the same four bytes that the generator patches. The
/// 65 535 square canvas needs 17 GB of RGBA8 despite a 93-byte file. The
/// store's 16 MiB byte cap cannot detect this area. The SVG tests the other
/// direction. The asset is valid, but the *scene* requests an impossible box.
/// This is the only size in this module that a caller chooses.
#[test]
fn a_surface_over_the_area_cap_is_refused_and_named_as_a_size() {
    let mut bomb = fixture("four.gif");
    bomb[6..10].fill(0xff);
    assert_eq!(
        Err(Undecodable::TooLarge(MediaFormat::Gif)),
        decode(MediaFormat::Gif, &bomb, None)
    );
    assert_eq!(
        Err(Undecodable::TooLarge(MediaFormat::Svg)),
        decode(MediaFormat::Svg, &fixture("two.svg"), Some((60_000, 60_000)))
    );
    // The cap is a limit, not a refusal of every large surface.
    assert!(decode(MediaFormat::Svg, &fixture("two.svg"), Some((4000, 4000))).is_ok());
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

#[test]
fn the_format_column_round_trips_every_variant() {
    for format in
        [MediaFormat::Png, MediaFormat::Jpeg, MediaFormat::Gif, MediaFormat::Svg, MediaFormat::Avif]
    {
        assert_eq!(Some(format), MediaFormat::parse(format.as_str()));
    }
    assert_eq!(None, MediaFormat::parse("webp"));
}

// ---- decoded-surface cache policy ----

/// This test covers both cache rules: eviction uses insertion order, and the
/// entry that `admit` receives remains in the cache.
///
/// The second rule needs explicit coverage. If eviction removed a fresh
/// oversized asset, the painter would report a visible image as absent and
/// decode it again on the next frame. Therefore, `admit` followed by `get` must
/// return the entry for every budget.
#[test]
fn the_byte_budget_evicts_the_oldest_and_never_what_it_just_took() {
    let mut held: ByteBudget<&str, ()> = ByteBudget::new(30);
    held.admit("a", (), 10);
    held.admit("b", (), 10);
    held.admit("c", (), 10);
    assert_eq!(30, held.footprint(), "three tens fit exactly");
    assert!(held.contains_key(&"a"), "nothing evicted while the budget holds");

    // One byte exceeds the budget, so only the oldest entry leaves.
    held.admit("d", (), 1);
    assert_eq!(21, held.footprint(), "b, c and d");
    assert!(!held.contains_key(&"a"), "the oldest went");
    for key in ["b", "c", "d"] {
        assert!(held.contains_key(&key), "{key} stayed");
    }

    // This value exceeds the whole budget. It remains available, and it is the only
    // entry left because older entries leave first.
    held.admit("huge", (), 500);
    assert_eq!(Some(&()), held.get(&"huge"), "an oversized asset is still served");
    assert_eq!(500, held.footprint());
    for key in ["b", "c", "d"] {
        assert!(!held.contains_key(&key), "{key} paid for it");
    }
}

/// A refusal has zero weight, so a broken Dictionary asset uses no pixel budget.
/// The bins state the weight, and a cached `Err` has weight zero.
#[test]
fn a_zero_weight_answer_is_cached_and_costs_no_budget() {
    let mut held: ByteBudget<&str, Result<(), Missing>> = ByteBudget::new(8);
    for key in ["one", "two", "three", "four", "five"] {
        held.admit(key, Err(Missing::NotStored), 0);
    }
    assert_eq!(0, held.footprint(), "five refusals cost nothing");
    for key in ["one", "two", "three", "four", "five"] {
        assert!(held.contains_key(&key), "{key} is a permanent answer and stays cached");
    }
}

/// This test checks rounded premultiplication at two boundaries. An opaque
/// channel must remain unchanged, and no channel can exceed its alpha.
///
/// These boundaries protect the invariant for every caller. Truncation would
/// make opaque white equal 254. That bug would create a per-platform hue if one
/// bin had a different copy. One shared function prevents that difference.
#[test]
fn premultiplying_rounds_and_never_exceeds_the_alpha() {
    for channel in 0..=255u8 {
        assert_eq!(channel, premultiplied(channel, 255), "opaque is untouched");
        assert_eq!(0, premultiplied(channel, 0), "no alpha is no ink");
        for alpha in 0..=255u8 {
            assert!(
                premultiplied(channel, alpha) <= alpha,
                "{channel} at alpha {alpha} escaped its own coverage",
            );
        }
    }
}
