//! Header probing and decoding, against the committed fixtures in
//! `tests/fixtures/media/` - one file per census format, every size
//! different and none of them square, so reading the right bytes off the
//! wrong format cannot pass.

use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every format the census found, and the size its generator gave it.
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
    // The `height: 1em` shape 字通 and 三省堂 both use: the width has to
    // come from the ratio, so the ratio is a column and not a derivation.
    let avif = probe(&fixture("five.avif")).unwrap();
    assert_eq!(4.0, avif.aspect, "480x120");
    let png = probe(&fixture("one.png")).unwrap();
    assert_eq!(12.0 / 7.0, png.aspect);
}

#[test]
fn an_animated_gif_is_sized_by_its_logical_screen_and_not_by_its_first_frame() {
    // The fixture is two 4x3 frames inside a 9x5 canvas. A browser lays
    // out the canvas, and the first frame this build will eventually paint
    // composites into it - so 9x5 is the intrinsic size and 4x3 is not.
    let got = probe(&fixture("four.gif")).unwrap();
    assert_eq!((9.0, 5.0), (got.width, got.height));
}

#[test]
fn an_svg_with_no_declared_size_takes_its_view_box() {
    let got = probe(&fixture("ratio.svg")).unwrap();
    assert_eq!((MediaFormat::Svg, 100.0, 40.0), (got.format, got.width, got.height));
}

/// One declared length plus a ratio gives the other length, which is CSS's
/// own resolution order for a replaced element.
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
    // Not an invented number: 300x150 is what every browser draws for a
    // replaced element with no intrinsic size and no intrinsic ratio. The
    // row exists, so ticket 12 renders the asset rather than its `alt`.
    let got = probe(br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#).unwrap();
    assert_eq!((300.0, 150.0), (got.width, got.height));
}

#[test]
fn absolute_svg_units_convert_to_pixels_and_relative_ones_name_no_size() {
    let inches = probe(br#"<svg width="1in" height="72pt"></svg>"#).unwrap();
    assert_eq!((96.0, 96.0), (inches.width, inches.height));

    // A percentage sizes against a containing box the asset does not have,
    // so it is *absent*, and the viewBox behind it is the intrinsic size.
    let relative = br#"<svg width="100%" height="50%" viewBox="0 0 20 10"></svg>"#;
    let got = probe(relative).unwrap();
    assert_eq!((20.0, 10.0), (got.width, got.height));
}

#[test]
fn an_attribute_probe_matches_whole_names_only() {
    // `stroke-width` must not answer a `width` probe, or a stroked gaiji
    // gets laid out at its pen width.
    let tag = br#"<svg stroke-width="9" viewBox="0 0 30 15"></svg>"#;
    let got = probe(tag).unwrap();
    assert_eq!((30.0, 15.0), (got.width, got.height));
}

/// Four bytes of an ISOBMFF box: length, type, payload.
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

/// An AVIF whose `ipco` lists a decoy `ispe` before the primary item's.
///
/// A real AVIF carries an alpha auxiliary item and often a thumbnail, each
/// with its own `ispe`, so "take the first one" sizes the picture from
/// whichever property happens to be listed first. `pitm` plus `ipma` is the
/// only thing that says which one is the picture.
fn avif_with_two_extents(associate: bool) -> Vec<u8> {
    let mut ipco = ispe(7, 3);
    ipco.extend(ispe(40, 25));
    let iprp = ibox(b"iprp", &ibox(b"ipco", &ipco));

    let mut meta = vec![0u8; 4];
    if associate {
        // `pitm`: version 0, flags 0, primary item 2.
        meta.extend(ibox(b"pitm", &[0, 0, 0, 0, 0, 2]));
        // `ipma`: version 0, flags 0, two entries, one narrow essential
        // property index each - item 1 to property 1, item 2 to property 2.
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
    // The single-item shape a dictionary converter emits: one `ispe`, and
    // no association to disambiguate. Sizing it is still better than
    // refusing it.
    let got = probe(&avif_with_two_extents(false)).unwrap();
    assert_eq!((7.0, 3.0), (got.width, got.height));
}

#[test]
fn a_recognised_container_with_no_readable_size_is_unreadable_and_not_a_panic() {
    // The committed `broken.png` is a signature with no IHDR behind it:
    // sniffed as a PNG, and unsizeable. The build skips it with a
    // diagnostic and writes no row.
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

/// A truncated or corrupt asset is the case the build must survive: an
/// archive is third-party bytes, and a panic in a probe would take the
/// whole rebuild down.
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

#[test]
fn decoding_premultiplies_alpha_into_the_surface() {
    // The fixture is 12x7 of pure blue at half alpha. Premultiplied, a
    // pixel is (0, 0, 128, 128) - painting the straight value would make
    // every gaiji too bright over a dark panel.
    let png = fixture("one.png");
    let s = decode(MediaFormat::Png, &png).expect("a true-colour PNG decodes");
    assert_eq!((12, 7), (s.w, s.h));
    assert_eq!(12 * 7 * 4, s.rgba.len());
    for px in s.rgba.as_chunks::<4>().0 {
        assert_eq!(&[0, 0, 128, 128], px);
    }
}

#[test]
fn grey_and_palette_pngs_widen_to_four_lanes() {
    for (name, w, h) in [("grey.png", 5u32, 4u32), ("unused.png", 3, 3)] {
        let s = decode(MediaFormat::Png, &fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((w, h), (s.w, s.h), "{name}");
        assert_eq!((w * h * 4) as usize, s.rgba.len(), "{name}");
    }
}

#[test]
fn a_format_this_build_cannot_rasterize_answers_no_decoder_rather_than_failing() {
    // A definite answer is what the `alt`-text ladder needs. The media row
    // still carries the right intrinsic size, so the line the image sits on
    // is laid out identically either way.
    for (name, format, ..) in SIZES {
        if format == MediaFormat::Png {
            continue;
        }
        assert_eq!(
            Err(Undecodable::NoDecoder(format)),
            decode(format, &fixture(name)),
            "{name}"
        );
    }
}

#[test]
fn a_ruined_png_answers_broken_rather_than_producing_pixels() {
    assert_eq!(
        Err(Undecodable::Broken(MediaFormat::Png)),
        decode(MediaFormat::Png, &fixture("broken.png"))
    );
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
