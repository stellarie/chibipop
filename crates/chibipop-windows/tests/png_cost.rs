//! PNG encode cost, on the WinRT encoder the screenshot action uses.
//!
//! Windows-only: it exercises the WinRT imaging stack. Elsewhere this
//! file compiles to zero tests.
#![cfg(windows)]

use std::time::Instant;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapEncoder, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Storage::Streams::InMemoryRandomAccessStream;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

const W: i32 = 1000;
const H: i32 = 200;
const RUNS: usize = 100;

fn uniform(w: i32, h: i32) -> Vec<u8> {
    vec![0xFFu8; (w as usize) * (h as usize) * 4]
}

fn noisy(w: i32, h: i32) -> Vec<u8> {
    let mut v = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut s: u32 = 0x1234_5678;
    for b in v.iter_mut() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (s >> 24) as u8;
    }
    v
}

fn text_like(w: i32, h: i32) -> Vec<u8> {
    let mut v = vec![0xFFu8; (w as usize) * (h as usize) * 4];
    for y in 0..h {
        for x in 0..w {
            let cell = ((x / 3) ^ (y / 5)) & 1;
            let stroke = x % 17 < 2 || y % 23 < 2;
            if cell == 1 && stroke {
                let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
                v[i] = 0x18;
                v[i + 1] = 0x18;
                v[i + 2] = 0x18;
            }
        }
    }
    v
}

fn encode_png(bgra: &[u8], w: i32, h: i32) -> windows::core::Result<u64> {
    let ibuffer = CryptographicBuffer::CreateFromByteArray(bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &ibuffer,
        BitmapPixelFormat::Bgra8,
        w,
        h,
        BitmapAlphaMode::Ignore,
    )?;
    let stream = InMemoryRandomAccessStream::new()?;
    let encoder =
        BitmapEncoder::CreateAsync(BitmapEncoder::PngEncoderId()?, &stream)?.join()?;
    encoder.SetSoftwareBitmap(&bitmap)?;
    encoder.FlushAsync()?.join()?;
    stream.Size()
}

#[test]
#[ignore = "timing, run by hand"]
fn png_encode_cost() {
    // SAFETY: this test thread has not initialised an apartment, and
    // BitmapEncoder needs one. The process is a test harness, so no other
    // code has established a conflicting apartment on this thread.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.unwrap();

    let cases = [
        ("uniform", uniform(W, H)),
        ("two-tone", text_like(W, H)),
        ("noise", noisy(W, H)),
    ];

    let mut worst_p95 = 0.0f64;
    let mut uniform_bytes = 0u64;
    let mut noise_bytes = 0u64;

    for (name, bgra) in &cases {
        let bytes = encode_png(bgra, W, H).expect("encode");
        if *name == "uniform" {
            uniform_bytes = bytes;
        }
        if *name == "noise" {
            noise_bytes = bytes;
        }

        let mut ms = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let t = Instant::now();
            let _ = encode_png(bgra, W, H).expect("encode");
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean: f64 = ms.iter().sum::<f64>() / RUNS as f64;
        let p95 = ms[RUNS * 95 / 100];
        worst_p95 = worst_p95.max(p95);
        println!(
            "{name:9} {bytes:>9} bytes  mean {mean:6.2}  p50 {:6.2}  p95 {p95:6.2}  max {:6.2}",
            ms[RUNS / 2],
            ms[RUNS - 1]
        );
    }

    assert!(
        noise_bytes > uniform_bytes * 10,
        "noise {noise_bytes} vs uniform {uniform_bytes}: pixels never reached the encoder"
    );
    println!("WORST-CASE p95: {worst_p95:.2} ms");
}
