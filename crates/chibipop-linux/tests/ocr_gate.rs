//! Run the Linux OCR quality gate (ARCHITECTURE.md#ocr-engine).
//!
//! The corpus has verified results for 152 crops under
//! `tests/fixtures/ocr-corpus/`. It is the same corpus that the Python
//! benchmark harness (`tools/ocr-bench/`) used to measure every candidate
//! engine. This test runs the ported pipeline on the full manifest and checks
//! three conditions:
//!
//! - **Absolute floors** (ARCHITECTURE.md#ocr-engine): horizontal CER <= 5 %
//!   with hit-scan >= 90 %, vertical CER <= 20 % with hit-scan >= 75 %.
//! - **Parity** with the harness's 1x values within +-3 pp. A resize rule, an
//!   overlap threshold, or an ONNX Runtime upgrade can cause silent drift. The
//!   gate catches that drift even when the absolute floors still pass.
//! - **Box fit**: a hit's box must outline its glyph. Hit-scan asks only whether
//!   the smallest box under the glyph center contains the character. Issue #92
//!   hovered a 100 px `新` and got `木` in a fragment box that passes that
//!   question. The harness has no fit metric. The gate scores unmasked crops
//!   at both scales.
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

use chibipop::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use chibipop::text::layout::{CaptureSize, OcrLine, Orientation, Resolved};
use chibipop::text::{CaptureMask, Frame, OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
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
/// A hit's box must also outline its glyph. Issue #92 hovered `新規` at about 100 px
/// and got `木`, a fragment of `新`, in a box that covered part of one glyph. The
/// hit-scan floors cannot see that failure because the fragment's box still contains
/// the glyph center. The fit floors match the hit-scan floors.
const HORIZONTAL_FIT_FLOOR: f64 = 0.90;
const VERTICAL_FIT_FLOOR: f64 = 0.75;
/// Set a generous limit. This catches a severe regression, not a slow runner.
/// Release measured 20.8 ms and debug measured 37 ms on developer hardware.
/// Container runs measured 88.0, 129.3, 132.5, 252.2, and 282.3 ms.
/// Runner class alone changes the result by about half. The product bar
/// (warm p50 <= 100 ms on developer hardware) does not apply here.
const LATENCY_P50_CEILING_MS: f64 = 500.0;

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
    let file = std::io::BufReader::new(std::fs::File::open(path).expect("open a corpus crop"));
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("read the PNG header");
    let mut rgb = vec![0u8; reader.output_buffer_size().expect("PNG buffer size")];
    let info = reader.next_frame(&mut rgb).expect("decode the PNG");
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
    let raw = std::fs::read_to_string(dir.join("manifest.json")).expect("read the corpus manifest");
    let manifest: serde_json::Value = serde_json::from_str(&raw).expect("parse the corpus manifest");
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

/// Box-fit tolerances. A box fits its glyph when it covers the middle half of the
/// ground-truth cell on both axes, its cross-axis thickness is at most 1.6 cells, and
/// its reading-axis length per character is at most 2.0 cells. A fragment (issue #92:
/// `木` inside `新`) fails the first rule. A line-tall or ruby-inclusive box fails the
/// second. Ground-truth cells are ink or DOM ranges, so a per-character width varies.
/// The tolerances leave room for that variation, not for a wrong box.
const FIT_CORE: f64 = 0.25;
const FIT_CROSS_MAX: f64 = 1.6;
const FIT_READ_MAX: f64 = 2.0;

/// Count boxes that fit their glyphs among hit boxes. Use the hit rule from
/// `hit_scan`. Each misfit names the character and its box for the report.
fn box_fit(chars: &[GtChar], boxes: &[PredBox], vertical: bool) -> (u32, u32, Vec<String>) {
    let (mut fit, mut hits) = (0, 0);
    let mut misfits = Vec::new();
    for ch in chars {
        let want = normalise(&ch.c);
        if want.is_empty() {
            continue;
        }
        let (px, py) = (ch.x + ch.w / 2.0, ch.y + ch.h / 2.0);
        let Some(b) = boxes
            .iter()
            .filter(|b| b.contains(px, py))
            .min_by(|a, b| a.area().total_cmp(&b.area()))
            .filter(|b| normalise(&b.text).contains(&want))
        else {
            continue;
        };
        hits += 1;
        let n = normalise(&b.text).chars().count().max(1) as f64;
        let covers_core = b.x <= ch.x + FIT_CORE * ch.w
            && b.x + b.w >= ch.x + (1.0 - FIT_CORE) * ch.w
            && b.y <= ch.y + FIT_CORE * ch.h
            && b.y + b.h >= ch.y + (1.0 - FIT_CORE) * ch.h;
        let sized = if vertical {
            b.w <= FIT_CROSS_MAX * ch.w && b.h / n <= FIT_READ_MAX * ch.h
        } else {
            b.h <= FIT_CROSS_MAX * ch.h && b.w / n <= FIT_READ_MAX * ch.w
        };
        if covers_core && sized {
            fit += 1;
        } else {
            misfits.push(format!(
                "{} at {},{} {}x{} hit {:?} at {},{} {}x{}",
                ch.c, ch.x, ch.y, ch.w, ch.h, b.text, b.x, b.y, b.w, b.h
            ));
        }
    }
    (fit, hits, misfits)
}

// ------------------------------------------------------------------- report

#[derive(Default, Clone)]
struct Tally {
    crops: u32,
    cer_sum: f64,
    cer_dropped_sum: f64,
    hits: u32,
    total: u32,
    /// Hits whose box fits the glyph, and the hits that the fit rule scored.
    /// Only unmasked crops count, at both scales.
    fit: u32,
    fit_hits: u32,
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

    fn fit(&self) -> f64 {
        if self.fit_hits == 0 { f64::NAN } else { f64::from(self.fit) / f64::from(self.fit_hits) }
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
    /// Every misfit as `(crop id, description)`. `run` prints them under `--nocapture`.
    misfits: Vec<(String, String)>,
}

static REPORT: LazyLock<Report> = LazyLock::new(run);

fn run() -> Report {
    let engine =
        MeikiOcr::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki")).expect("open the bundled models");
    let corpus = load_corpus();
    assert_eq!(152, corpus.len(), "the committed corpus must match the benchmark");

    let mut by_slice: BTreeMap<(i64, String), Tally> = BTreeMap::new();
    let mut horizontal = Tally::default();
    let mut vertical = Tally::default();
    let mut masked = Tally::default();
    let (mut smoke_pred, mut smoke_gt) = (String::new(), String::new());
    let mut misfits: Vec<(String, String)> = Vec::new();

    for crop in &corpus {
        let lines = engine.recognise(&crop.pixels, crop.pw, crop.ph).expect("recognise a corpus crop");
        let (pred, boxes) = flatten(&lines);
        let gt = normalise(&crop.text);
        let pred = normalise(&pred);
        let crop_cer = cer(&gt, &pred);
        let (hits, total) = hit_scan(&crop.chars, &boxes);
        // The fit rule has no scale term, so both scales count. Exclude masked crops
        // because the mask cuts boxes by design.
        let (fit, fit_hits, crop_misfits) = if crop.mask.is_none() {
            box_fit(&crop.chars, &boxes, crop.slice == "vertical")
        } else {
            (0, 0, Vec::new())
        };
        misfits.extend(crop_misfits.into_iter().map(|m| (crop.id.clone(), m)));

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
        bucket.fit += fit;
        bucket.fit_hits += fit_hits;

        let slice = by_slice.entry((crop.scale, crop.slice.clone())).or_default();
        slice.crops += 1;
        slice.cer_sum += crop_cer;
        slice.cer_dropped_sum += cer_dropped;
        slice.hits += hits;
        slice.total += total;
        slice.fit += fit;
        slice.fit_hits += fit_hits;

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

    let mut table = String::from("\nOCR gate - measured against the Python harness 1x reference\n");
    table.push_str("  slice                 crops    CER%    hit%    fit%\n");
    for ((scale, name), t) in &by_slice {
        table.push_str(&format!(
            "  {:<12} {scale}x   {:>5}  {:>6.2}  {:>6.2}  {:>6.2}\n",
            name,
            t.crops,
            t.cer() * 100.0,
            t.hit() * 100.0,
            t.fit() * 100.0
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
        "  ---\n  horizontal box fit    {:>5} hits  {:>6.2} % fit   (both scales, unmasked; floor {:.2})\n",
        horizontal.fit_hits,
        horizontal.fit() * 100.0,
        HORIZONTAL_FIT_FLOOR * 100.0
    ));
    table.push_str(&format!(
        "  vertical box fit      {:>5} hits  {:>6.2} % fit   (both scales, unmasked; floor {:.2})\n",
        vertical.fit_hits,
        vertical.fit() * 100.0,
        VERTICAL_FIT_FLOOR * 100.0
    ));
    table.push_str(&format!(
        "  warm p50 on j1_1x: {latency_p50_ms:.1} ms (reference 21.8 ms, ceiling {LATENCY_P50_CEILING_MS:.0} ms)\n"
    ));
    println!("{table}");
    for (id, misfit) in &misfits {
        println!("  misfit {id}: {misfit}");
    }

    Report { by_slice, horizontal, vertical, masked, smoke_pred, smoke_gt, latency_p50_ms, table, misfits }
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

/// Check that a horizontal hit's box outlines its glyph, not a fragment or the line.
#[test]
fn horizontal_boxes_fit_their_glyphs() {
    let got = REPORT.horizontal.fit();
    assert!(got >= HORIZONTAL_FIT_FLOOR, "horizontal box fit {:.2} % < 90 %{}", got * 100.0, REPORT.table);
}

#[test]
fn vertical_boxes_fit_their_glyphs() {
    let got = REPORT.vertical.fit();
    assert!(got >= VERTICAL_FIT_FLOOR, "vertical box fit {:.2} % < 75 %{}", got * 100.0, REPORT.table);
}

/// Check the issue-92 shape on real pixels. `smoke_2x` holds three 78-86 px glyphs.
/// One box must hit each glyph and cover it. A fragment box (`木` inside `新`) hits
/// the center but fails the fit rule.
#[test]
fn large_glyphs_are_boxed_whole() {
    let smoke = REPORT.by_slice.get(&(2, "smoke".to_string())).expect("smoke_2x slice");
    let misfits: Vec<&str> = REPORT
        .misfits
        .iter()
        .filter(|(id, _)| id == "smoke_2x")
        .map(|(_, misfit)| misfit.as_str())
        .collect();
    assert_eq!(
        (smoke.fit, smoke.fit_hits, smoke.total),
        (3, 3, 3),
        "every smoke_2x glyph must have a box that covers it. Misfits: {misfits:?}{}",
        REPORT.table
    );
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
    near(REPORT.masked.cer_dropped(), REF_MASKED_CER_DROPPED, "masked CER after the layout drops clipped words");
    near(REPORT.masked.hit(), REF_MASKED_HIT, "masked hit-scan");
}

/// Check the sparse fixture at the cursor and crop edge. This case eliminated
/// PP-OCRv5. The frame contains only three glyphs.
#[test]
fn the_sparse_fixture_is_read_exactly() {
    assert_eq!(REPORT.smoke_gt, REPORT.smoke_pred, "the three-glyph smoke crop must return its text verbatim{}", REPORT.table);
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

// -------------------------------------------------------------- large text
//
// Issue #92: text at or above the height of the 500x100 capture box. The screens
// under `tests/fixtures/large-text/` come from `scripts/render-large-text.py`. The
// engine reads them through `TextSource`, the daemon's own hover path, so the
// capture box, every pass, and the scan rects are the ones a user gets.

fn large_text_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/large-text")
}

struct Screen {
    id: String,
    size: i64,
    hover: PhysPoint,
    /// The text from the hovered glyph to the line end, or to the box edge when
    /// `prefix` is set.
    expect: String,
    prefix: bool,
    /// Ink boxes in screen pixels.
    chars: Vec<GtChar>,
    pixels: Vec<u8>,
    w: i32,
    h: i32,
}

fn load_large_text() -> Vec<Screen> {
    let dir = large_text_dir();
    let raw = std::fs::read_to_string(dir.join("manifest.json")).expect("read the large-text manifest");
    let manifest: serde_json::Value = serde_json::from_str(&raw).expect("parse the large-text manifest");
    manifest["screens"]
        .as_array()
        .expect("manifest.screens")
        .iter()
        .map(|e| {
            let (pixels, w, h) = load_bgra(&dir.join(e["file"].as_str().expect("file")));
            Screen {
                id: e["id"].as_str().expect("id").to_string(),
                size: e["size"].as_i64().expect("size"),
                hover: PhysPoint {
                    x: e["hover"]["x"].as_i64().expect("hover.x") as i32,
                    y: e["hover"]["y"].as_i64().expect("hover.y") as i32,
                },
                expect: e["expect"].as_str().expect("expect").to_string(),
                prefix: e["prefix"].as_bool().unwrap_or(false),
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
                pixels,
                w,
                h,
            }
        })
        .collect()
}

/// A `RegionCapture` over one screen. Pixels outside the screen are black, as the
/// wlr-screencopy backend leaves them. The screen is the one output.
struct ScreenCapture {
    pixels: Vec<u8>,
    w: i32,
    h: i32,
}

impl RegionCapture for ScreenCapture {
    fn grab(&mut self, region: PhysRect) -> anyhow::Result<Frame> {
        let mut buf = vec![0u8; (region.w * region.h * 4) as usize];
        for row in 0..region.h {
            let y = region.y + row;
            if y < 0 || y >= self.h {
                continue;
            }
            let x0 = region.x.max(0);
            let x1 = (region.x + region.w).min(self.w);
            if x1 <= x0 {
                continue;
            }
            let src = ((y * self.w + x0) * 4) as usize;
            let dst = ((row * region.w) + (x0 - region.x)) as usize * 4;
            let len = ((x1 - x0) * 4) as usize;
            buf[dst..dst + len].copy_from_slice(&self.pixels[src..src + len]);
        }
        Ok(Frame { buf, w: region.w, h: region.h, source: "screen", fallback: None, unchanged: false })
    }

    fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
        PhysRect { x: 0, y: 0, w: self.w, h: self.h }
    }
}

struct LargeRead {
    id: String,
    size: i64,
    expect: String,
    prefix: bool,
    hovered: GtChar,
    resolved: Option<Resolved>,
    scan: Vec<ScanRect>,
}

static LARGE: LazyLock<Vec<LargeRead>> = LazyLock::new(read_large_text);

/// Hover every screen with the daemon's default OCR settings and three passes, so
/// forward tiles finish a line that the box clips on the reading axis.
fn read_large_text() -> Vec<LargeRead> {
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki");
    let settings = SettingsSnapshot {
        max_passes: 3,
        upscale: 1,
        prefer_vertical: false,
        capture: CaptureSize::default(),
        scan_alphanumeric: true,
        discard_furigana: true,
    };
    load_large_text()
        .into_iter()
        .map(|screen| {
            let engine = MeikiOcr::open(&models).expect("open the bundled models");
            let capture = ScreenCapture { pixels: screen.pixels, w: screen.w, h: screen.h };
            let mut source = TextSource::new(Box::new(capture), Box::new(engine), settings);
            let (resolved, scan, _) = source
                .resolve_at_tiled_scanned(screen.hover, true, CaptureMask::NONE)
                .expect("read a large-text screen");
            let hovered = screen
                .chars
                .into_iter()
                .find(|c| screen.expect.starts_with(c.c.as_str()) && {
                    let (px, py) = (f64::from(screen.hover.x), f64::from(screen.hover.y));
                    c.x <= px && px < c.x + c.w && c.y <= py && py < c.y + c.h
                })
                .expect("the hovered glyph's ink box");
            println!("large text {}: {} px, scan {:?}", screen.id, screen.size, scan);
            if let Some(r) = &resolved {
                println!("large text {}: text {:?} at {} anchor {:?}", screen.id, r.span.text, r.span.cursor_byte_offset, r.span.anchor);
            }
            LargeRead {
                id: screen.id,
                size: screen.size,
                expect: screen.expect,
                prefix: screen.prefix,
                hovered,
                resolved,
                scan,
            }
        })
        .collect()
}

fn large(id: &str) -> &'static LargeRead {
    LARGE.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("large-text screen {id}"))
}

/// The returned text must include the hovered glyph and the rest of its line verbatim.
/// A long line must extend at least to the box edge. Forward tiles read the rest, and their
/// reach at large sizes is the engine's, not this gate's.
fn read_whole(read: &LargeRead) {
    let resolved = read
        .resolved
        .as_ref()
        .unwrap_or_else(|| panic!("{} ({} px): no hit. Scan {:?}", read.id, read.size, read.scan));
    let tail = &resolved.span.text[resolved.span.cursor_byte_offset..];
    let context = format!("{} ({} px): scan {:?}", read.id, read.size, read.scan);
    if read.prefix {
        assert!(tail.starts_with(&read.expect), "{context}: got {tail:?}, expected prefix {:?}", read.expect);
    } else {
        assert_eq!(tail, read.expect, "{context}");
    }
}

#[test]
fn large_text_that_fits_the_box_is_read_whole() {
    read_whole(large("shinki_100"));
}

#[test]
fn large_text_taller_than_the_box_is_read_whole() {
    read_whole(large("shinki_130"));
    read_whole(large("shinki_160"));
}

#[test]
fn low_stroke_kanji_taller_than_the_box_are_read_whole() {
    read_whole(large("nihongo_130"));
}

/// The issue #92 follow-up screenshot: a news line in BIZ UDPGothic, white on black.
/// The engine returns no words at 100 px in the 100 px box. The 400 px box reads the
/// line after it scales the crop down.
#[test]
fn light_on_dark_large_text_is_read_whole() {
    read_whole(large("katsu_biz_100"));
    read_whole(large("katsu_noto_100"));
}

/// Bold BIZ UDPGothic at 125 px: the 100 px box reads a cut `活` as `沽`. A cut
/// read is not an answer. The 200 px box reads the glyph whole.
#[test]
fn a_cut_glyph_is_not_the_answer_when_a_grown_box_reads_it_whole() {
    read_whole(large("katsu_biz_bold_125"));
}

/// Cursor placement. A cursor near the top or the bottom of a large glyph puts the
/// box edge through the glyph. The engine then returns incorrect text that fits the
/// box, such as `サ千子ペナ` for the top half of `活発な`. A hit word that touches
/// one edge of the box and is at least half the box thick is a cut read, not an answer.
#[test]
fn a_cursor_near_the_top_or_bottom_of_a_large_glyph_still_reads_it_whole() {
    read_whole(large("katsu_biz_110_top"));
    read_whole(large("katsu_biz_110_bottom"));
}

/// The scan rects stay on the hovered line, and the anchor outlines the glyph.
#[test]
fn large_text_scan_rects_face_the_hovered_line() {
    for read in LARGE.iter() {
        let context = format!("{} ({} px): scan {:?}", read.id, read.size, read.scan);
        let Some(resolved) = &read.resolved else { panic!("{context}") };
        assert_eq!(resolved.orientation, Orientation::Horizontal, "{context}");
        let anchor = resolved.span.anchor;
        let centre = anchor.center();
        for tile in read.scan.iter().filter(|s| s.kind == ScanKind::Tile) {
            let t = tile.rect;
            assert!(t.y <= centre.y && centre.y < t.y + t.h, "{context}");
        }
        let boxed = PredBox {
            text: read.expect.chars().next().expect("a hovered glyph").to_string(),
            x: f64::from(anchor.x),
            y: f64::from(anchor.y),
            w: f64::from(anchor.w),
            h: f64::from(anchor.h),
        };
        let (fit, hits, misfits) = box_fit(std::slice::from_ref(&read.hovered), &[boxed], false);
        assert_eq!((fit, hits), (1, 1), "{context}. Misfits: {misfits:?}");
    }
}
