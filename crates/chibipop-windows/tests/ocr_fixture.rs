//! This integration test uses the real Windows OCR engine.
//!
//! It reads a raw BGRA fixture in the format that `capture.rs` produces.
//!
//! The test skips when the engine is unavailable.
//! This lets a fresh clone pass `cargo test`.
//! This is the approved pattern that the M1 golden corpus uses.
//!
//! Windows-only: this test calls WinRT OCR.
//! Elsewhere, this file compiles to zero tests.
#![cfg(windows)]

use chibipop::text::layout::resolve;

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
    let built = chibipop_windows::text::ocr::WinrtOcr::new("ja");
    let source = match built {
        Ok(source) => source,
        Err(_) => {
            eprintln!("SKIP: no Japanese OCR engine available");
            return;
        }
    };

    let lines = chibipop_windows::text::ocr::recognise(source.engine(), &buf, FIX_W, FIX_H)
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

    // Every word must have a non-degenerate box. Hit-scan needs this box.
    for line in &lines {
        for word in &line.words {
            assert!(word.rect.w > 0 && word.rect.h > 0, "degenerate box: {:?}", word.rect);
        }
    }

    // The center of the first recognized word must resolve to
    // that same word.
    let first = &lines[0].words[0];
    let hit = resolve(&lines, first.rect.center(), true).expect("centre of a word must resolve");
    assert!(
        hit.span.text[hit.span.cursor_byte_offset..].starts_with(&first.text),
        "expected the span to start at {:?}, got {:?}",
        first.text,
        &hit.span.text[hit.span.cursor_byte_offset..]
    );
}
