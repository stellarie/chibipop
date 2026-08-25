//! ADR-0009's standing quality gate for the Linux OCR engine.
//!
//! The 152-crop ground-truthed corpus under `tests/fixtures/ocr-corpus/`
//! is the same one the Python benchmark harness (`tools/ocr-bench/`)
//! measured every candidate engine on, manifest and all. This runs the
//! ported pipeline over it and holds the result to two things at once:
//!
//! - **Absolute floors**, from the ADR: horizontal CER <= 5 % with
//!   hit-scan >= 90 %, vertical CER <= 20 % with hit-scan >= 75 %.
//! - **Parity** with the harness's 1x numbers to within +-3 pp, so a
//!   silent drift in the port - a resize rounding rule, an overlap
//!   threshold, an ONNX Runtime upgrade - reds the gate even while the
//!   absolute floors still hold.
//!
//! Every metric below is computed the way `bench/common.py` computes it,
//! down to picking the *smallest* box containing the cursor point and
//! scoring masked crops after dropping predictions that touch the mask.
//! A metric that drifts from the harness makes the parity band meaningless.
//!
//! Latency is asserted only as a generous ceiling: runners vary, and the
//! product bar (warm p50 <= 100 ms on developer hardware) is not something
//! a shared CI machine can speak to.
#![cfg(target_os = "linux")]

use chibipop::text::layout::OcrLine;
use chibipop::text::OcrEngine;
use chibipop_linux::ocr::MeikiOcr;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------- reference
//
// Measured by `python -m bench.run_one --config meiki` on 2026-08-23 and
// stored in `tools/ocr-bench/results/meiki.json`; the aggregation below is
// `bench/report.py`'s (CER averaged per crop, hit-scan pooled over
// characters). Quoted in docs/research/ocr-benchmark-results.md and
// ADR-0009. These are the 1x numbers - ADR-0009's amendment made 1x the
// only thing the Linux adapter feeds.

/// Slices `smoke`, `horizontal`, `mixed` and `small` at 1x: 7 crops.
const REF_HORIZONTAL_CER: f64 = 0.0181;
/// 116 of 122 characters.
const REF_HORIZONTAL_HIT: f64 = 0.9508;
/// The `vertical` slice at 1x: one 16-glyph column.
const REF_VERTICAL_CER: f64 = 0.1250;
/// 13 of 16 characters.
const REF_VERTICAL_HIT: f64 = 0.8125;
/// All 136 ADR-0008 masked variants, scored after dropping predictions
/// whose boxes touch the mask - what chibipop's layout actually keeps.
const REF_MASKED_CER_DROPPED: f64 = 0.1410;
/// 1435 of 1542 characters.
const REF_MASKED_HIT: f64 = 0.9306;

/// The band the port must stay inside, in proportion (3 pp).
///
/// Worth knowing when this trips: the vertical slice is a single 16-glyph
/// crop, so its CER moves in 6.25 pp steps. Vertical parity is therefore an
/// exact-match assertion in practice, which is the point - ADR-0009 asks
/// for the vertical slice to be re-measured on any upstream model change.
const PARITY_BAND: f64 = 0.03;

// -------------------------------------------------------------------- gate

const HORIZONTAL_CER_CEILING: f64 = 0.05;
const HORIZONTAL_HIT_FLOOR: f64 = 0.90;
const VERTICAL_CER_CEILING: f64 = 0.20;
const VERTICAL_HIT_FLOOR: f64 = 0.75;
/// Generous on purpose: the reference is 21.8 ms warm on developer
/// hardware and a shared runner is nowhere near that.
const LATENCY_P50_CEILING_MS: f64 = 250.0;

/// Slices that are not the vertical one. ADR-0009 gates them together.
const HORIZONTAL_SLICES: [&str; 4] = ["smoke", "horizontal", "mixed", "small"];

// ------------------------------------------------------------------ corpus

struct Crop {
    id: String,
    slice: String,
    scale: i64,
    w: f64,
    h: f64,
    text: String,
    chars: Vec<GtChar>,
    mask: Option<Mask>,
    pixels: Vec<u8>,
    pw: i32,
    ph: i32,
}

struct GtChar {
    c: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct Mask {
    pos: String,
    rect: [f64; 4],
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr-corpus")
}

/// The PNG as the capture layer would hand it over: tightly packed BGRA,
/// top-down, alpha junk. Every corpus crop is 8-bit RGB.
fn load_bgra(path: &Path) -> (Vec<u8>, i32, i32) {
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("opening a corpus crop"));
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("reading the PNG header");
    let mut rgb = vec![0u8; reader.output_buffer_size().expect("PNG buffer size")];
    let info = reader.next_frame(&mut rgb).expect("decoding the PNG");
    assert_eq!(png::ColorType::Rgb, info.color_type, "{}", path.display());
    assert_eq!(png::BitDepth::Eight, info.bit_depth, "{}", path.display());

    let (w, h) = (info.width as usize, info.height as usize);
    let mut bgra = Vec::with_capacity(w * h * 4);
    let (pixels, _) = rgb[..w * h * 3].as_chunks::<3>();
    for p in pixels {
        bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    (bgra, w as i32, h as i32)
}

fn load_corpus() -> Vec<Crop> {
    let dir = corpus_dir();
    let raw = std::fs::read_to_string(dir.join("manifest.json")).expect("reading the corpus manifest");
    let manifest: serde_json::Value = serde_json::from_str(&raw).expect("parsing the corpus manifest");
    let entries = manifest["crops"].as_array().expect("manifest.crops");

    entries
        .iter()
        .map(|e| {
            let (pixels, pw, ph) = load_bgra(&dir.join(e["file"].as_str().expect("file")));
            Crop {
                id: e["id"].as_str().expect("id").to_string(),
                slice: e["slice"].as_str().expect("slice").to_string(),
                scale: e["scale"].as_i64().expect("scale"),
                w: e["w"].as_f64().expect("w"),
                h: e["h"].as_f64().expect("h"),
                text: e["text"].as_str().expect("text").to_string(),
                chars: e["chars"]
                    .as_array()
                    .expect("chars")
                    .iter()
                    .map(|c| GtChar {
                        c: c["c"].as_str().expect("c").to_string(),
                        x: c["x"].as_f64().expect("x"),
                        y: c["y"].as_f64().expect("y"),
                        w: c["w"].as_f64().expect("w"),
                        h: c["h"].as_f64().expect("h"),
                    })
                    .collect(),
                mask: e["mask"].as_object().map(|m| Mask {
                    pos: m["pos"].as_str().expect("mask.pos").to_string(),
                    rect: {
                        let r = m["rect"].as_array().expect("mask.rect");
                        [
                            r[0].as_f64().expect("rect"),
                            r[1].as_f64().expect("rect"),
                            r[2].as_f64().expect("rect"),
                            r[3].as_f64().expect("rect"),
                        ]
                    },
                }),
                pixels,
                pw,
                ph,
            }
        })
        .collect()
}

// ------------------------------------------------------------------ metrics
//
// `bench/common.py`, transcribed. The hyphen rule is chibipop's own
// `layout.rs normalise()`; NFKC and whitespace-stripping are the benchmark
// protocol from docs/research/linux-japanese-ocr.md.

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    for c in text.nfkc().filter(|c| !c.is_whitespace()) {
        let c = match c {
            '-' | '\u{2010}' | '\u{2013}' | '\u{2014}' if prev.is_some_and(is_kana) => '\u{30FC}',
            other => other,
        };
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Edit distance in characters - the sum of the substitutions, deletions
/// and insertions the harness reports separately.
fn edit_distance(gt: &[char], pred: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=pred.len()).collect();
    let mut cur = vec![0usize; pred.len() + 1];
    for (i, g) in gt.iter().enumerate() {
        cur[0] = i + 1;
        for (j, p) in pred.iter().enumerate() {
            cur[j + 1] = if g == p {
                prev[j]
            } else {
                1 + prev[j].min(prev[j + 1]).min(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[pred.len()]
}

fn cer(gt: &str, pred: &str) -> f64 {
    let g: Vec<char> = gt.chars().collect();
    let p: Vec<char> = pred.chars().collect();
    if g.is_empty() {
        return if p.is_empty() { 0.0 } else { p.len() as f64 };
    }
    edit_distance(&g, &p) as f64 / g.len() as f64
}

/// One predicted chunk, in crop pixels - the harness's `Box`.
struct PredBox {
    text: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl PredBox {
    fn contains(&self, px: f64, py: f64) -> bool {
        self.x <= px && px < self.x + self.w && self.y <= py && py < self.y + self.h
    }

    fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    fn intersects(&self, rect: [f64; 4]) -> bool {
        let [rx, ry, rw, rh] = rect;
        !(self.x + self.w <= rx || rx + rw <= self.x || self.y + self.h <= ry || ry + rh <= self.y)
    }
}

/// Flattens engine output into the harness's two views: reading-order text
/// and the per-chunk boxes hit-scan resolves against.
fn flatten(lines: &[OcrLine]) -> (String, Vec<PredBox>) {
    let mut text = String::new();
    let mut boxes = Vec::new();
    for line in lines {
        for word in &line.words {
            text.push_str(&word.text);
            boxes.push(PredBox {
                text: word.text.clone(),
                x: f64::from(word.rect.x),
                y: f64::from(word.rect.y),
                w: f64::from(word.rect.w),
                h: f64::from(word.rect.h),
            });
        }
    }
    (text, boxes)
}

/// The cursor at every ground-truth character centre. A hit is the
/// *smallest* box containing that point carrying that character.
///
/// An engine that returned no geometry at all scores neither hits nor
/// misses - it leaves the pool, exactly as the harness treats a
/// geometry-less engine. Counting its characters as misses would quietly
/// change the denominator and make the parity band meaningless.
fn hit_scan(chars: &[GtChar], boxes: &[PredBox]) -> (u32, u32) {
    if boxes.is_empty() {
        return (0, 0);
    }
    let (mut hits, mut total) = (0, 0);
    for ch in chars {
        let want = normalise(&ch.c);
        if want.is_empty() {
            continue; // whitespace ground truth is not hoverable
        }
        total += 1;
        let (px, py) = (ch.x + ch.w / 2.0, ch.y + ch.h / 2.0);
        let best = boxes
            .iter()
            .filter(|b| b.contains(px, py))
            .min_by(|a, b| a.area().total_cmp(&b.area()));
        if let Some(best) = best {
            if normalise(&best.text).contains(&want) {
                hits += 1;
            }
        }
    }
    (hits, total)
}

// ------------------------------------------------------------------- report

#[derive(Default, Clone)]
struct Tally {
    crops: u32,
    cer_sum: f64,
    cer_dropped_sum: f64,
    hits: u32,
    total: u32,
}

impl Tally {
    fn cer(&self) -> f64 {
        if self.crops == 0 { f64::NAN } else { self.cer_sum / f64::from(self.crops) }
    }

    fn cer_dropped(&self) -> f64 {
        if self.crops == 0 { f64::NAN } else { self.cer_dropped_sum / f64::from(self.crops) }
    }

    fn hit(&self) -> f64 {
        if self.total == 0 { f64::NAN } else { f64::from(self.hits) / f64::from(self.total) }
    }
}

struct Report {
    by_slice: BTreeMap<(i64, String), Tally>,
    horizontal: Tally,
    vertical: Tally,
    masked: Tally,
    smoke_pred: String,
    smoke_gt: String,
    latency_p50_ms: f64,
    table: String,
}

static REPORT: LazyLock<Report> = LazyLock::new(run);

fn run() -> Report {
    let engine =
        MeikiOcr::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki")).expect("opening the bundled models");
    let corpus = load_corpus();
    assert_eq!(152, corpus.len(), "the committed corpus is the benchmark's, unchanged");

    let mut by_slice: BTreeMap<(i64, String), Tally> = BTreeMap::new();
    let mut horizontal = Tally::default();
    let mut vertical = Tally::default();
    let mut masked = Tally::default();
    let (mut smoke_pred, mut smoke_gt) = (String::new(), String::new());

    for crop in &corpus {
        let lines = engine.recognise(&crop.pixels, crop.pw, crop.ph).expect("recognising a corpus crop");
        let (pred, boxes) = flatten(&lines);
        let gt = normalise(&crop.text);
        let pred = normalise(&pred);
        let crop_cer = cer(&gt, &pred);
        let (hits, total) = hit_scan(&crop.chars, &boxes);

        // The harness's production-equivalent score for a masked crop:
        // chibipop's layout drops words whose rects touch the mask
        // (ADR-0008 "the mask boundary is a capture edge"), so boundary
        // garbage with honest geometry never reaches the lookup.
        let mut cer_dropped = crop_cer;
        if let Some(mask) = &crop.mask {
            if mask.pos != "outside" && !boxes.is_empty() {
                let [x0, y0, mw, mh] = mask.rect;
                let (x1, y1) = ((x0 + mw).min(crop.w), (y0 + mh).min(crop.h));
                let (x0, y0) = (x0.max(0.0), y0.max(0.0));
                let clipped = [x0, y0, x1 - x0, y1 - y0];
                let kept: String =
                    boxes.iter().filter(|b| !b.intersects(clipped)).map(|b| b.text.as_str()).collect();
                cer_dropped = cer(&gt, &normalise(&kept));
            }
        }

        let bucket = if crop.mask.is_some() {
            &mut masked
        } else if crop.slice == "vertical" {
            &mut vertical
        } else {
            &mut horizontal
        };
        // Only 1x feeds the parity aggregates; the masked variants are all
        // 2x renders and are gated on their own numbers.
        if crop.mask.is_some() || crop.scale == 1 {
            bucket.crops += 1;
            bucket.cer_sum += crop_cer;
            bucket.cer_dropped_sum += cer_dropped;
            bucket.hits += hits;
            bucket.total += total;
        }

        let slice = by_slice.entry((crop.scale, crop.slice.clone())).or_default();
        slice.crops += 1;
        slice.cer_sum += crop_cer;
        slice.cer_dropped_sum += cer_dropped;
        slice.hits += hits;
        slice.total += total;

        if crop.id == "smoke_1x" {
            smoke_pred = pred.clone();
            smoke_gt = gt.clone();
        }
    }

    // Warm p50 on the representative horizontal crop, the same one the
    // harness times as `j1_1x`.
    let bench = corpus.iter().find(|c| c.id == "j1_1x").expect("j1_1x");
    for _ in 0..3 {
        engine.recognise(&bench.pixels, bench.pw, bench.ph).expect("warm-up");
    }
    let mut samples: Vec<f64> = (0..15)
        .map(|_| {
            let t = Instant::now();
            engine.recognise(&bench.pixels, bench.pw, bench.ph).expect("timed run");
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    let latency_p50_ms = samples[samples.len() / 2];

    let mut table = String::from("\nOCR gate - measured vs the Python harness 1x reference\n");
    table.push_str("  slice                 crops    CER%    hit%\n");
    for ((scale, name), t) in &by_slice {
        table.push_str(&format!(
            "  {:<12} {scale}x   {:>5}  {:>6.2}  {:>6.2}\n",
            name,
            t.crops,
            t.cer() * 100.0,
            t.hit() * 100.0
        ));
    }
    table.push_str(&format!(
        "  ---\n  horizontal family 1x  {:>5}  {:>6.2}  {:>6.2}   (reference {:.2} / {:.2})\n",
        horizontal.crops,
        horizontal.cer() * 100.0,
        horizontal.hit() * 100.0,
        REF_HORIZONTAL_CER * 100.0,
        REF_HORIZONTAL_HIT * 100.0
    ));
    table.push_str(&format!(
        "  vertical 1x           {:>5}  {:>6.2}  {:>6.2}   (reference {:.2} / {:.2})\n",
        vertical.crops,
        vertical.cer() * 100.0,
        vertical.hit() * 100.0,
        REF_VERTICAL_CER * 100.0,
        REF_VERTICAL_HIT * 100.0
    ));
    table.push_str(&format!(
        "  masked (dropped)      {:>5}  {:>6.2}  {:>6.2}   (reference {:.2} / {:.2})\n",
        masked.crops,
        masked.cer_dropped() * 100.0,
        masked.hit() * 100.0,
        REF_MASKED_CER_DROPPED * 100.0,
        REF_MASKED_HIT * 100.0
    ));
    table.push_str(&format!(
        "  warm p50 on j1_1x: {latency_p50_ms:.1} ms (reference 21.8 ms, ceiling {LATENCY_P50_CEILING_MS:.0} ms)\n"
    ));
    println!("{table}");

    Report { by_slice, horizontal, vertical, masked, smoke_pred, smoke_gt, latency_p50_ms, table }
}

fn near(measured: f64, reference: f64, what: &str) {
    assert!(
        (measured - reference).abs() <= PARITY_BAND,
        "{what}: {:.2} % is more than {:.0} pp off the harness's {:.2} %{}",
        measured * 100.0,
        PARITY_BAND * 100.0,
        reference * 100.0,
        REPORT.table
    );
}

// ------------------------------------------------------------------- gates

#[test]
fn horizontal_text_clears_the_cer_ceiling() {
    let got = REPORT.horizontal.cer();
    assert!(got <= HORIZONTAL_CER_CEILING, "horizontal CER {:.2} % > 5 %{}", got * 100.0, REPORT.table);
}

#[test]
fn horizontal_text_clears_the_hit_scan_floor() {
    let got = REPORT.horizontal.hit();
    assert!(got >= HORIZONTAL_HIT_FLOOR, "horizontal hit-scan {:.2} % < 90 %{}", got * 100.0, REPORT.table);
}

#[test]
fn vertical_text_clears_its_beta_ceiling() {
    let got = REPORT.vertical.cer();
    assert!(got <= VERTICAL_CER_CEILING, "vertical CER {:.2} % > 20 %{}", got * 100.0, REPORT.table);
}

#[test]
fn vertical_text_clears_its_beta_hit_scan_floor() {
    let got = REPORT.vertical.hit();
    assert!(got >= VERTICAL_HIT_FLOOR, "vertical hit-scan {:.2} % < 75 %{}", got * 100.0, REPORT.table);
}

#[test]
fn horizontal_accuracy_matches_the_python_harness() {
    near(REPORT.horizontal.cer(), REF_HORIZONTAL_CER, "horizontal CER");
    near(REPORT.horizontal.hit(), REF_HORIZONTAL_HIT, "horizontal hit-scan");
}

#[test]
fn vertical_accuracy_matches_the_python_harness() {
    near(REPORT.vertical.cer(), REF_VERTICAL_CER, "vertical CER");
    near(REPORT.vertical.hit(), REF_VERTICAL_HIT, "vertical hit-scan");
}

/// ADR-0008's masked sweep, the robustness half of ticket 31: the engine
/// must not fall apart when part of the crop is painted over, once the
/// boundary words chibipop already discards are discarded.
#[test]
fn masked_crops_match_the_python_harness() {
    near(REPORT.masked.cer_dropped(), REF_MASKED_CER_DROPPED, "masked CER after dropping clipped words");
    near(REPORT.masked.hit(), REF_MASKED_HIT, "masked hit-scan");
}

/// The sparse fixture is the cursor-at-crop-edge case that eliminated
/// PP-OCRv5 (ADR-0009): three glyphs, nothing else in the frame.
#[test]
fn the_sparse_fixture_is_read_exactly() {
    assert_eq!(REPORT.smoke_gt, REPORT.smoke_pred, "the three-glyph smoke crop must come back verbatim{}", REPORT.table);
    let smoke = REPORT.by_slice.get(&(1, "smoke".to_string())).expect("smoke slice");
    assert_eq!(smoke.hits, smoke.total, "every smoke glyph must be hoverable{}", REPORT.table);
}

/// A ceiling, not a measurement: runners vary by an order of magnitude and
/// the product bar lives on developer hardware.
#[test]
fn one_crop_stays_under_the_ci_latency_ceiling() {
    assert!(
        REPORT.latency_p50_ms <= LATENCY_P50_CEILING_MS,
        "warm p50 {:.1} ms > {LATENCY_P50_CEILING_MS:.0} ms{}",
        REPORT.latency_p50_ms,
        REPORT.table
    );
}

/// Prints the measured-vs-reference table. Run with `-- --nocapture`.
#[test]
fn the_gate_covers_every_slice_of_the_committed_corpus() {
    print!("{}", REPORT.table);
    for slice in HORIZONTAL_SLICES.iter().chain(["vertical", "masked"].iter()) {
        let scale = if *slice == "masked" { 2 } else { 1 };
        assert!(
            REPORT.by_slice.contains_key(&(scale, (*slice).to_string())),
            "the corpus lost its {slice} slice"
        );
    }
}
