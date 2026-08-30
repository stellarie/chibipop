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

/// One pixel, as four premultiplied bytes.
fn px(s: &Surface, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * s.w + x) * 4) as usize;
    s.rgba[at..at + 4].try_into().expect("a surface is w * h * 4 bytes")
}

/// Within `tol` on every channel, which is what a lossy format allows.
fn near(got: [u8; 4], want: [u8; 4], tol: i32, what: &str) {
    let off = got.iter().zip(want).map(|(&g, w)| (i32::from(g) - i32::from(w)).abs()).max();
    assert!(off <= Some(tol), "{what}: {got:?} is not within {tol} of {want:?}");
}

#[test]
fn decoding_premultiplies_alpha_into_the_surface() {
    // The fixture is 12x7 of pure blue at half alpha. Premultiplied, a
    // pixel is (0, 0, 128, 128) - painting the straight value would make
    // every gaiji too bright over a dark panel.
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

/// Every format the census found now reaches pixels, which is the whole
/// point of the ticket: before this, `decode` refused four of five and a
/// real gaiji painted as an outlined box.
///
/// One sampled pixel per format, at its own generator colour, so reading
/// the right bytes off the wrong format cannot pass. The tolerances are
/// the formats' own: JPEG at quality 80 and AVIF at `-q 40` are lossy, and
/// the AVIF fixture is `magick gradient:red-blue`, which runs top to
/// bottom - so the ends are two rows in from each edge.
#[test]
fn every_census_format_decodes_to_a_surface() {
    let jpeg = decode(MediaFormat::Jpeg, &fixture("three.jpg"), None).expect("jpeg");
    assert_eq!((23, 11), (jpeg.w, jpeg.h));
    near(px(&jpeg, 11, 5), [0xa0, 0x50, 0x30, 255], 6, "a flat #a05030 jpeg");

    let gif = decode(MediaFormat::Gif, &fixture("four.gif"), None).expect("gif");
    assert_eq!((9, 5), (gif.w, gif.h));
    assert_eq!([255, 0, 0, 255], px(&gif, 1, 1), "the first frame is red");

    // 64x32 declared over a 0 0 128 64 viewBox, so the rect at viewBox
    // (8,8)-(120,56) lands at pixel (4,4)-(60,28).
    let svg = decode(MediaFormat::Svg, &fixture("two.svg"), None).expect("svg");
    assert_eq!((64, 32), (svg.w, svg.h));
    assert_eq!([0x30, 0x50, 0xa0, 255], px(&svg, 32, 16), "inside the rect");
    assert_eq!([0, 0, 0, 0], px(&svg, 1, 1), "outside it, and transparent");

    let avif = decode(MediaFormat::Avif, &fixture("five.avif"), None).expect("avif");
    assert_eq!((480, 120), (avif.w, avif.h));
    near(px(&avif, 240, 2), [255, 0, 0, 255], 24, "the red end of the gradient");
    near(px(&avif, 240, 117), [0, 0, 255, 255], 24, "the blue end");
}

/// The spec asks for the first frame, and the media row records the
/// logical screen - so the frame has to composite into that canvas rather
/// than be stretched to it.
#[test]
fn an_animated_gif_decodes_its_first_frame_into_the_logical_screen() {
    // Two 4x3 frames, red then blue, inside a 9x5 canvas.
    let gif = decode(MediaFormat::Gif, &fixture("four.gif"), None).expect("gif");
    assert_eq!((9, 5), (gif.w, gif.h), "the canvas, not the frame");
    for px in gif.rgba.as_chunks::<4>().0 {
        assert_ne!(&[0, 0, 255, 255], px, "the second frame must not be painted");
    }
    // Outside the frame rectangle the canvas stays transparent, which is
    // what a later frame would composite onto.
    assert_eq!([0, 0, 0, 0], px(&gif, 8, 4));
    assert_eq!([255, 0, 0, 255], px(&gif, 3, 2), "the last pixel of the frame");
}

/// `Tint::Raster` exists because a vector has no pixels until something
/// picks a size, and this is the end of that wire: the pair core quantised
/// out of the resolved box is the pixmap the bin gets.
#[test]
fn an_svg_rasterizes_at_the_size_the_scene_asked_for() {
    let bytes = fixture("two.svg");
    let asked = decode(MediaFormat::Svg, &bytes, Some((16, 8))).expect("svg at a size");
    assert_eq!((16, 8), (asked.w, asked.h));
    assert_eq!(16 * 8 * 4, asked.rgba.len());
    // The rect scales with the box rather than being cropped out of it.
    assert_eq!([0x30, 0x50, 0xa0, 255], px(&asked, 8, 4));

    let intrinsic = decode(MediaFormat::Svg, &bytes, None).expect("svg at its own size");
    assert_eq!((64, 32), (intrinsic.w, intrinsic.h));

    // A raster asset has its own pixels, so the same request changes
    // nothing about it - `at` is a vector's alone.
    let png = decode(MediaFormat::Png, &fixture("one.png"), Some((99, 99))).expect("png");
    assert_eq!((12, 7), (png.w, png.h));
}

/// A vector with no declared size still rasterizes: its `viewBox` is the
/// intrinsic size the media row recorded, which is the shape a gaiji drawn
/// from a font glyph arrives in - all 35 of this library's SVG assets.
#[test]
fn an_svg_sized_only_by_its_view_box_still_reaches_pixels() {
    let s = decode(MediaFormat::Svg, &fixture("ratio.svg"), None).expect("svg");
    assert_eq!((100, 40), (s.w, s.h));
    assert_eq!([0xa0, 0x50, 0x30, 255], px(&s, 50, 20));
}

/// Premultiplied is the contract `Surface` states, and `rgb <= a` is the
/// invariant tiny-skia's own pixel type enforces - a surface that broke it
/// would blend brighter than the asset every frame.
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

/// Half of every fixture, one format at a time. A definite `Err` is what
/// the `alt`-text ladder acts on, and the media row still carries the
/// right intrinsic size, so the line the image sits on is laid out
/// identically either way.
#[test]
fn a_truncated_asset_of_any_format_answers_undecodable_rather_than_pixels() {
    for (name, format, ..) in SIZES {
        let bytes = fixture(name);
        let half = &bytes[..bytes.len() / 2];
        assert!(decode(format, half, None).is_err(), "half of {name} decoded");
    }
}

/// A declared area over the cap is refused before anything allocates it.
///
/// Both halves matter. The GIF is the real fixture with its logical screen
/// descriptor overwritten - the same four bytes the generator patches, and
/// the same shape the store's own 16 MiB byte cap cannot see: a 65 535
/// square canvas is 17 GB of RGBA8 out of 93 bytes of file. The SVG is the
/// other direction, where the asset is fine and the *scene* asked for an
/// impossible box, which is the one size in this module that is a choice.
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
    // And the bound is a bound, not a refusal of everything large.
    assert!(decode(MediaFormat::Svg, &fixture("two.svg"), Some((4000, 4000))).is_ok());
}

/// A decoder is new attack surface over untrusted dictionary bytes, and it
/// runs at paint time: a panic here takes the daemon down mid-hover rather
/// than costing one diagnostic line at build time.
///
/// Every prefix of the header region, where a length or a dimension lives,
/// plus a stride over the payload. Bounded on purpose - a decoder that
/// survives thousands of mutations of five real containers is not going to
/// be talked into a panic by one more, and this runs three times per CI
/// job.
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
