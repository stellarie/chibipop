//! The platform-neutral half of the OCR pipeline: upscale mapping and
//! hit-scan resolution composing correctly. The companion test that drives
//! the real Windows OCR engine over the BGRA fixture lives in
//! `crates/chibipop-windows/tests/ocr_fixture.rs`.

use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::text::layout::{map_from_upscaled, resolve, OcrLine, OcrWord};

#[test]
fn mapping_and_resolution_compose_correctly() {
    // A word at (200,100) in a 2x-upscaled image of a region whose origin is
    // (1000,500) must land at virtual-desktop (1100,550) and resolve there.
    let mapped = OcrLine {
        words: vec![OcrWord {
            text: "食".to_string(),
            rect: map_from_upscaled(
                PhysRect { x: 200, y: 100, w: 40, h: 40 },
                PhysPoint { x: 1000, y: 500 },
                2,
            ),
        }],
    };
    let got = resolve(&[mapped], PhysPoint { x: 1110, y: 560 }, true).expect("must resolve");
    assert_eq!("食", got.span.text);
    assert_eq!(0, got.span.cursor_byte_offset);
    assert_eq!(1100, got.span.anchor.x);
    assert_eq!(550, got.span.anchor.y);
}
