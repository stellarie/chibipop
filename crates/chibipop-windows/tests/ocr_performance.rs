#![cfg(windows)]

use chibipop::text::OcrEngine;
use chibipop_windows::plugin::{host, manifest, text::PluginText};
use chibipop_windows::text::ocr::WinrtOcr;
use std::time::Instant;
use std::hash::{Hash, Hasher};

#[test]
#[ignore = "Runs installed OCR engines for performance measurements"]
fn fixed_pixels_engine_latency() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/japanese_bgra.bin");
    let (pixels, width, height, expected) = match std::env::var_os("CHIBIPOP_BENCH_BMP") {
        Some(path) => {
            let bmp = std::fs::read(path).unwrap();
            let field = |at| i32::from_le_bytes(bmp[at..at + 4].try_into().unwrap());
            let offset = field(10) as usize;
            let width = field(18);
            let height = field(22);
            assert!(width > 0 && height != 0);
            let depth = u16::from_le_bytes(bmp[28..30].try_into().unwrap()) as usize;
            assert!(depth == 24 || depth == 32);
            let pitch = (width as usize * depth).div_ceil(32) * 4;
            let mut pixels = Vec::new();
            for y in 0..height.unsigned_abs() as usize {
                let row = if height > 0 { height as usize - 1 - y } else { y };
                for x in 0..width as usize {
                    let at = offset + row * pitch + x * (depth / 8);
                    pixels.extend_from_slice(&[bmp[at], bmp[at + 1], bmp[at + 2], 255]);
                }
            }
            (pixels, width, height.abs(), '学')
        }
        None => (std::fs::read(path).unwrap(), 400, 120, '昨'),
    };
    let mut engines: Vec<Box<dyn OcrEngine>> = vec![Box::new(WinrtOcr::new("ja").unwrap())];
    if let Some(dir) = std::env::var_os("CHIBIPOP_BENCH_PLUGIN") {
        let dir = std::path::PathBuf::from(dir);
        let spec = manifest::parse(&std::fs::read_to_string(dir.join("plugin.toml")).unwrap()).unwrap();
        let started = Instant::now();
        let process = host::spawn(&spec, &dir).unwrap();
        println!("BENCH startup engine={} ms={:.3}", spec.name, started.elapsed().as_secs_f64() * 1000.0);
        engines.push(Box::new(PluginText::new(process, &spec)));
    }
    for engine in engines {
        for scale in [1, 2] {
            let (buf, w, h) = chibipop::text::source::upscale_by(&pixels, width, height, scale);
            let mut reference = None;
            for sample in 0..13 {
                let started = Instant::now();
                let lines = engine.recognise(&buf, w, h).unwrap();
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                let text: String = lines.iter().flat_map(|line| &line.words).map(|word| word.text.as_str()).collect();
                assert!(text.contains(expected), "{}: {text}", engine.name());
                assert!(lines.iter().flat_map(|line| &line.words).all(|word| word.rect.w > 0 && word.rect.h > 0));
                if let Some(previous) = &reference {
                    assert_eq!(previous, &lines, "fixed pixels must retain text and geometry");
                }
                let mut geometry = std::collections::hash_map::DefaultHasher::new();
                format!("{lines:?}").hash(&mut geometry);
                reference = Some(lines);
                println!("BENCH engine={} scale={scale} sample={sample} ms={elapsed:.3} geometry_hash={:016x} text={text}", engine.name(), geometry.finish());
            }
        }
    }
}
