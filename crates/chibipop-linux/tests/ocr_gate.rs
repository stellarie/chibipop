//! Run the Linux OCR quality gate (ARCHITECTURE.md#ocr-engine).
//!
//! The corpus has verified results for 152 crops under
//! `tests/fixtures/ocr-corpus/`. It is the same corpus that the Python
//! benchmark harness (`tools/ocr-bench/`) used to measure every candidate
//! engine. This test runs the ported pipeline on the full manifest and checks
//! two conditions:
//!
//! - **Absolute floors** (ARCHITECTURE.md#ocr-engine): horizontal CER <= 5 %
//!   with hit-scan >= 90 %, vertical CER <= 20 % with hit-scan >= 75 %.
//! - **Parity** with the harness's 1x values within +-3 pp. A resize rule, an
//!   overlap threshold, or an ONNX Runtime upgrade can cause silent drift. The
//!   gate catches that drift even when the absolute floors still pass.
//!
//! Each metric uses the method from `bench/common.py`. It selects the *smallest*
//! box that contains the cursor point. It drops predictions that touch the mask
//! before it scores masked crops. A metric that differs from the harness makes
//! the parity band meaningless.
//!
//! The gate checks latency only against a generous ceiling. Runner speed differs.
//! A shared CI machine cannot represent the product bar of warm p50 <= 100 ms
//! on developer hardware.
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
// The benchmark measured these values with `python -m bench.run_one --config meiki`
// on 2026-08-23 and stored them in `tools/ocr-bench/results/meiki.json`.
// The aggregation below follows `bench/report.py`: it averages CER per crop and
// pools hit-scan over characters. The values appear in
// `docs/research/ocr-benchmark-results.md`. These are 1x values. The Linux
// adapter uses 1x in production.

/// Reference CER for the `smoke`, `horizontal`, `mixed`, and `small` slices at
/// 1x. The set has 7 crops.
const REF_HORIZONTAL_CER: f64 = 0.0181;
/// The harness hit 116 of 122 characters.
const REF_HORIZONTAL_HIT: f64 = 0.9508;
/// Reference CER for the `vertical` slice at 1x. The slice has one 16-glyph
/// column.
const REF_VERTICAL_CER: f64 = 0.1250;
/// The harness hit 13 of 16 characters.
const REF_VERTICAL_HIT: f64 = 0.8125;
/// Reference CER for all 136 masked variants. Drop predictions whose boxes
/// touch the mask before score comparison, as chibipop's layout does.
const REF_MASKED_CER_DROPPED: f64 = 0.1410;
/// The harness hit 1435 of 1542 characters.
const REF_MASKED_HIT: f64 = 0.9306;

/// The allowed difference from the port to the reference, as a proportion.
///
/// The vertical slice has one 16-glyph crop. Its CER changes in 6.25 pp steps.
/// Therefore, vertical parity requires an exact result in practice. Re-measure
/// the vertical slice after every upstream model change.
const PARITY_BAND: f64 = 0.03;

// -------------------------------------------------------------------- gate

const HORIZONTAL_CER_CEILING: f64 = 0.05;
const HORIZONTAL_HIT_FLOOR: f64 = 0.90;
const VERTICAL_CER_CEILING: f64 = 0.20;
const VERTICAL_HIT_FLOOR: f64 = 0.75;
/// Set a generous limit. This catches a severe regression, not a slow runner.
/// Release measured 20.8 ms and debug measured 37 ms on developer hardware.
/// Three debug runs on ubuntu-24.04 measured 88.0, 129.3, and 132.5 ms.
/// Runner class alone changes the result by about half. The product bar
/// (warm p50 <= 100 ms on developer hardware) does not apply here.
const LATENCY_P50_CEILING_MS: f64 = 250.0;

/// The non-vertical slices that the gate scores together.
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

/// Convert a PNG to the capture layer's input: tightly packed, top-down BGRA
/// with unused alpha. Every corpus crop uses 8-bit RGB.
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
// This code follows `bench/common.py`. The hyphen rule comes from chibipop's
// `layout.rs normalise()`. NFKC and whitespace removal come from the benchmark
// protocol in `docs/research/linux-japanese-ocr.md`.

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

/// Return edit distance in characters. The harness counts substitutions, deletions,
/// and insertions as parts of this distance.
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

/// A predicted chunk in crop pixels. This matches the harness's `Box`.
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

/// Convert engine output into the two views that the harness uses: text in line
/// order and per-chunk boxes for `hit_scan`.
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

/// Check the cursor at each ground-truth character centre. A hit is the *smallest*
/// box that contains that point and carries that character.
///
/// If an engine returns no geometry, score neither hits nor misses. Leave those
/// characters out of the pool, as the harness does. Do not count them as misses.
/// That would change the denominator and make the parity band meaningless.
fn hit_scan(chars: &[GtChar], boxes: &[PredBox]) -> (u32, u32) {
    if boxes.is_empty() {
        return (0, 0);
    }
    let (mut hits, mut total) = (0, 0);
    for ch in chars {
        let want = normalise(&ch.c);
        if want.is_empty() {
            continue; // Ignore whitespace because it has no hover target.
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

        // Use the harness score for a masked crop.
        // chibipop's layout drops words whose rects touch the mask.
        // (ARCHITECTURE.md#capture-and-masking) calls the mask boundary a capture edge.
        // Boundary garbage with valid geometry therefore does not reach the lookup.
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
        // Add only 1x crops to parity totals. Score masked variants with their own totals.
        // All masked variants use 2x renders.
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

    // Measure warm p50 on the representative horizontal crop. The harness calls this
    // crop `j1_1x`.
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

/// Check the masked sweep, the robustness part of this gate. The engine must keep
/// its quality when a mask covers part of a crop. The layout already drops
/// boundary words.
#[test]
fn masked_crops_match_the_python_harness() {
    near(REPORT.masked.cer_dropped(), REF_MASKED_CER_DROPPED, "masked CER after dropping clipped words");
    near(REPORT.masked.hit(), REF_MASKED_HIT, "masked hit-scan");
}

/// Check the sparse fixture at the cursor and crop edge. This case eliminated
/// PP-OCRv5. The frame contains only three glyphs.
#[test]
fn the_sparse_fixture_is_read_exactly() {
    assert_eq!(REPORT.smoke_gt, REPORT.smoke_pred, "the three-glyph smoke crop must come back verbatim{}", REPORT.table);
    let smoke = REPORT.by_slice.get(&(1, "smoke".to_string())).expect("smoke slice");
    assert_eq!(smoke.hits, smoke.total, "every smoke glyph must be hoverable{}", REPORT.table);
}

/// Apply a ceiling, not a measurement. Runner speed varies by an order of
/// magnitude. The product bar applies only to developer hardware.
#[test]
fn one_crop_stays_under_the_ci_latency_ceiling() {
    assert!(
        REPORT.latency_p50_ms <= LATENCY_P50_CEILING_MS,
        "warm p50 {:.1} ms > {LATENCY_P50_CEILING_MS:.0} ms{}",
        REPORT.latency_p50_ms,
        REPORT.table
    );
}

/// Print the measured and reference table. Run with `-- --nocapture`.
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
