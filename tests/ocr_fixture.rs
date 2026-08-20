//! Integration test against the real Windows OCR engine, using a raw BGRA
//! fixture in exactly the format `capture.rs` produces.
//!
//! Skips when the engine is unavailable, so a fresh clone still passes
//! `cargo test` — the same approved pattern the M1 golden corpus uses.

use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::text::layout::{map_from_upscaled, resolve, CaptureSize, OcrLine, OcrWord};

const FIX_W: i32 = 400;
const FIX_H: i32 = 120;

fn fixture() -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/japanese_bgra.bin");
    std::fs::read(p).ok()
}

#[test]
fn real_engine_reads_the_fixture_and_boxes_every_character() {
    let Some(buf) = fixture() else {
        eprintln!("SKIP: tests/fixtures/japanese_bgra.bin not present");
        return;
    };
    let built =
        chibipop::text::ocr::OcrTextSource::new(1, false, CaptureSize::default(), true, "ja");
    let source = match built {
        Ok(source) => source,
        Err(_) => {
            eprintln!("SKIP: no Japanese OCR engine available");
            return;
        }
    };

    let lines = source
        .recogniser()
        .recognise(&buf, FIX_W, FIX_H)
        .expect("the engine must accept a Bgra8 buffer in capture.rs's format");

    let assembled: String = lines
        .iter()
        .flat_map(|l| l.words.iter())
        .map(|w| w.text.as_str())
        .collect();
    assert!(
        assembled.contains('昨') && assembled.contains('日'),
        "expected 昨日 in the recognised text, got {assembled:?}"
    );

    // Every word must carry a non-degenerate box - that is what hit-scan needs.
    for line in &lines {
        for word in &line.words {
            assert!(word.rect.w > 0 && word.rect.h > 0, "degenerate box: {:?}", word.rect);
        }
    }

    // Pointing at the centre of the first recognised character must resolve to
    // that same character.
    let first = &lines[0].words[0];
    let hit = resolve(&lines, first.rect.center(), true).expect("centre of a word must resolve");
    assert!(
        hit.span.text[hit.span.cursor_byte_offset..].starts_with(&first.text),
        "expected the span to start at {:?}, got {:?}",
        first.text,
        &hit.span.text[hit.span.cursor_byte_offset..]
    );
}

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
