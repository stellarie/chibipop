# OCR candidate benchmark results

Executes the benchmark protocol from [Research: Japanese OCR engines on Linux](./linux-japanese-ocr.md)
("Benchmark shortlist"), plus the masked-fixture sweep
([popup capture exclusion](../../ARCHITECTURE.md#capture-and-masking)).
Run date: 2026-08-24. **Numbers only — no engine choice is made here.**

Harness: [`tools/ocr-bench/`](../../tools/ocr-bench/README.md) (fully
reproducible: `setup.sh` → `gen_corpus` → `run_all`; models hash-recorded in
`models/SHA256SUMS.txt`, raw per-crop JSON in `results/`).

## The four decision inputs, answered with numbers

1. **meikiocr beta vertical: measurably degraded, not broken.** Vertical CER
   12.5 % (1×) / 18.8 % (2×) vs 0–3.5 % horizontal; it consistently dropped the
   first glyph and the trailing `。` of the vertical fixture (`吾輩は…` →
   `輩は…`, and `猫`→`描` at 2×). Vertical hit-scan 81.2 % (1×) / 75.0 % (2×) vs
   94–100 % horizontal.
2. **PP-OCRv5 word boxes are good enough for hit-scan on line text, but the
   detector fails sparse crops.** `return_word_box` works in the RapidOCR
   wrapper and yields per-character boxes for CJK: hit-scan 94.4–100 %
   horizontal, 87.5 % vertical. But the smoke fixture (three 36 pt glyphs in a
   400×120 crop, the exact production byte format) is a **total detection
   miss**: `'d'` at 1×, empty at 2× → 100 % CER, 0 % hit-scan. The
   research doc's isolated-short-text caveat is real and reproduces on our
   smallest fixture.
3. **Tesseract clears the CER floor on clean horizontal text but its geometry
   is unusable for hit-scan.** jpn fast: 0–1.8 % CER horizontal — Windows-class
   on this corpus. But LSTM symbol boxes are CTC-blob artifacts (a 50 px-wide
   "学" box followed by a 4 px "生" sliver), giving 33–62 % horizontal hit-scan;
   `jpn_vert` symbol boxes come back fully degenerate (x=0, w=0) and even
   word-level boxes only reach 50–56 %. Vertical at 2× (nearest-neighbour)
   grows a hallucinated second column (`足馬は黄下…` prefix). tess-best is
   *worse* than fast at 2× (7.0 % vs 1.8 % horizontal CER).
4. **manga-ocr warm latency lands far under 500 ms.** Greedy p50 ≈ 177 ms /
   p95 ≈ 224 ms and beam-4 p50 ≈ 224 ms / p95 ≈ 271 ms on the production
   1000×200 crop, CPU-only — the "release builds are much faster than the
   published ~2 s debug numbers" hypothesis confirmed (~10×). The actual
   blockers are elsewhere: ≤16 px text fed as a whole 500×100 crop is
   confabulated wholesale (86–94 % CER of fluent, wrong Japanese), and black
   interior masks trigger fluent bridging hallucinations (+128 pp CER,
   ~12 inserted chars/crop).

Masked-sweep headline: **masking is viable for the box engines, and fill color
matters enormously for some engines.** After chibipop's clipped-word exclusion
(dropping predicted boxes that intersect the mask), meikiocr keeps the interior-
mask penalty at +11.6 pp and PP-OCRv5 at +1.9 pp — but black/gray fills poison
Tesseract (+43–55 pp even after dropping) and black poisons manga-ocr whole-crop
mode. White or crop-mean fill with either edge treatment is safe across all
four engines; the 1 px feather bought nothing measurable (and doubled
manga-ocr's whole-crop damage). The capture-guard fallback looks unnecessary
for box-engine pipelines on this evidence.

---

## Methodology

**Corpus** (`tools/ocr-bench/bench/gen_corpus.py`, 152 crops):

- `docs/fixtures/ocr-corpus.html` rendered untouched (iframe-embedded) by
  headless Chromium at device-scale 1; per-character geometry captured from DOM
  `Range.getBoundingClientRect`. Blocks used: `j1`, `outlined` (white-on-dark,
  1 px stroke), `j2` (slices *horizontal*); `alnum` (*mixed* JA+alphanumeric);
  `vert` (*vertical* 縦書き). Two wrapper-added small-glyph lines (*small*):
  16 px JA and 12 px mixed (`HPが50%を切ったらSkill発動…`).
- Crops are production-shaped: 500×100 horizontal, 100×500 vertical, trimmed
  to whole characters (region past the last fully contained glyph painted with
  the local line background; rows/columns outside the text band blanked to
  remove the corpus page's red debug labels). Ground truth = the characters
  actually inside the crop, with their pixel boxes.
- `tests/fixtures/japanese_bgra.bin` (400×120 tightly packed BGRA, alpha 0xFF —
  byte-for-byte the production capture format) decoded directly as the *smoke*
  slice; its three glyph boxes recovered by ink projection and visually
  verified.
- Every crop also exists at 2× via **nearest-neighbour** upscale, mirroring
  `src/text/capture.rs upscale_by` (production OCRs the 2× image).
- **Masked variants**, on every 2× base crop: popup-sized flat rect ≈ ⅓ of
  crop area at three positions (straddling one crop edge / fully interior /
  adjacent-but-outside as a no-op control) × four fills (black, white,
  mid-gray 128, per-crop mean color) × two edges (hard, 1 px 50 % feather
  ring). Masked ground truth = characters whose boxes do not intersect the
  mask (chibipop drops mask-intersecting words).

**Engines** (one process per configuration; models pinned + hashed):

| config | what runs |
|---|---|
| meiki | meikiocr 0.3.4 pipeline, det 960×544 + rec 960×32 + vrec 32×480, CPU EP, per-char boxes |
| ppocrv5 | PP-OCRv5 mobile det+rec ONNX (RapidOCR v3.9.2 distribution) via `rapidocr`, cls off, `return_word_box=True` |
| tess-fast / tess-best | Tesseract 5.5.3 C API in-process (ctypes; no subprocess in any latency number), `jpn` PSM 7 for horizontal / `jpn_vert` PSM 5 for vertical, `preserve_interword_spaces=1`, symbol-level boxes (word-level for vertical — see caveats) |
| manga-greedy / manga-beam | mayocream manga-ocr ONNX export via onnxruntime, greedy and batched beam k=4, fed (a) whole crops and (b) meikiocr detector line boxes |

**Metrics:**

- **CER** = Levenshtein distance / |GT| after NFKC → whitespace-strip →
  chibipop's `normalise()` hyphen-after-kana rule (mirrored from
  `src/text/layout.rs`), on both GT and prediction, lines joined in reading
  order (vertical columns right-to-left).
- **Hit-scan success**: cursor simulated at each GT character's center; success
  iff the smallest predicted box containing that point includes the character.
  This is `hit_scan`'s geometry contract, not IoU. Skipped for manga-ocr (no
  geometry, by architecture).
- **Latency**: cold = first call after construction in a fresh process (one
  process per size); warm p50/p95 over 100 iterations per crop size after 3
  warmup calls, sequential, otherwise-idle machine. Sizes: 500×100, 1000×200,
  100×500, 200×1000, 400×120, 800×240 (manga cold: 2× sizes only).
- **Construction** = engine object build incl. model load; **peak RSS** =
  `ru_maxrss` of the full per-config process (accuracy + latency work).
- **Masked deltas**: ΔCER vs the same engine's unmasked 2× base; plus
  **ΔCER-dropped** — recomputed after removing predicted boxes intersecting
  the mask, which is what survives chibipop's clipped-word exclusion
  (box engines only); `boxes-in-mask` and inserted-char counts as boundary-
  hallucination signals.

## Environment

- CPU: AMD Ryzen 7 9800X3D (8C/16T), CPU-only (`CPUExecutionProvider`
  everywhere), Arch Linux, kernel 7.0.11-zen1.
- Python 3.14.5 venv; onnxruntime 1.29.0; rapidocr 3.9.2; meikiocr 0.3.4;
  numpy 2.5.2; opencv-python-headless 5.0.0.93. Tesseract 5.5.3 +
  leptonica 1.87.0 (conda-forge, user-space). Chromium 149.0.7827.53
  (corpus rendering only). Full freeze: `tools/ocr-bench/results/env.txt`.
- Model files and SHA-256 hashes: `tools/ocr-bench/models/SHA256SUMS.txt`.
  The PP-OCRv5 det/rec hashes match the SHA-256s RapidOCR publishes in its
  `default_models.yaml` for the v3.9.2 model tag.
- **Latency caveat (Python vs `ort`)**: the harness drives the same ONNX
  Runtime C library that the Rust `ort` crate wraps, and Tesseract through its
  C API, so inference cost is representative; per-call Python pre/post-
  processing overhead (numpy/OpenCV glue, RapidOCR's pipeline plumbing) is
  included in the numbers and would shrink somewhat in a Rust port. Tesseract
  numbers involve no process spawning.

## CER (%) by corpus slice — unmasked, lower is better

Slicing: *smoke* = BGRA bin fixture; *horizontal* = j1/outlined/j2; *mixed* =
alnum; *small* = 16 px + 12 px lines; *vertical* = vert.

### 1× scale

| config | smoke | horizontal | mixed | small | vertical |
|---|---|---|---|---|---|
| meiki | 0.0 | 1.8 | 0.0 | 3.7 | 12.5 |
| ppocrv5 | 100.0 | 3.8 | 14.3 | 3.7 | 25.0 |
| tess-fast | 0.0 | 1.8 | 0.0 | 3.7 | 0.0 |
| tess-best | 0.0 | 1.8 | 0.0 | 3.7 | 0.0 |
| manga-greedy (whole) | 0.0 | 5.3 | 9.5 | 86.4 | 0.0 |
| manga-greedy (meiki-lines) | 0.0 | 10.9 | 19.1 | 46.6 | 0.0 |
| manga-beam (whole) | 0.0 | 1.8 | 9.5 | 92.3 | 0.0 |
| manga-beam (meiki-lines) | 0.0 | 10.9 | 14.3 | 46.6 | 0.0 |

### 2× scale (the production path)

| config | smoke | horizontal | mixed | small | vertical |
|---|---|---|---|---|---|
| meiki | 0.0 | 3.5 | 0.0 | 3.7 | 18.8 |
| ppocrv5 | 100.0 | 0.0 | 0.0 | 3.7 | 12.5 |
| tess-fast | 0.0 | 1.8 | 0.0 | 5.6 | 31.2 |
| tess-best | 0.0 | 7.0 | 0.0 | 11.1 | 43.8 |
| manga-greedy (whole) | 0.0 | 3.5 | 9.5 | 94.1 | 12.5 |
| manga-greedy (meiki-lines) | 0.0 | 16.1 | 19.1 | 42.9 | 6.2 |
| manga-beam (whole) | 0.0 | 0.0 | 4.8 | 90.4 | 12.5 |
| manga-beam (meiki-lines) | 0.0 | 16.1 | 14.3 | 52.2 | 6.2 |

## Hit-scan success (%) — cursor at each GT char center, unmasked

manga-ocr is excluded (returns no geometry, by architecture).

### 1× scale

| config | smoke | horizontal | mixed | small | vertical |
|---|---|---|---|---|---|
| meiki | 100.0 | 94.4 | 100.0 | 93.2 | 81.2 |
| ppocrv5 | 0.0 | 94.4 | 100.0 | 93.2 | 87.5 |
| tess-fast | 33.3 | 40.7 | 52.4 | 52.3 | 56.2 |
| tess-best | 33.3 | 33.3 | 47.6 | 36.4 | 56.2 |

### 2× scale

| config | smoke | horizontal | mixed | small | vertical |
|---|---|---|---|---|---|
| meiki | 100.0 | 94.4 | 100.0 | 90.9 | 75.0 |
| ppocrv5 | —* | 96.3 | 100.0 | 93.2 | 87.5 |
| tess-fast | 100.0 | 38.9 | 61.9 | 47.7 | 56.2 |
| tess-best | 33.3 | 35.2 | 52.4 | 38.6 | 50.0 |

\* ppocrv5 detected nothing on smoke_2x (no boxes at all → excluded rather
than 0/3; at 1× it produced one wrong box, scored 0.0).

## Latency per crop size (ms, CPU) — cold / warm p50 / warm p95

100 warm iterations per size; cold from a fresh process per size.

| config | 500×100 | 1000×200 | 100×500 | 200×1000 | 400×120 (smoke) | 800×240 |
|---|---|---|---|---|---|---|
| meiki | 33 / 21.8 / 24.3 | 27 / 21.9 / 23.3 | 27 / 20.8 / 22.3 | 26 / 20.9 / 23.1 | 27 / 21.6 / 27.5 | 26 / 21.7 / 23.6 |
| ppocrv5 | 397 / 249.0 / 277.9 | 370 / 248.9 / 331.2 | 354 / 252.3 / 306.6 | 346 / 251.4 / 283.3 | 248 / 168.4 / 188.4 | 243 / 166.0 / 192.5 |
| tess-fast | 10 / 9.2 / 9.6 | 14 / 12.8 / 13.4 | 13 / 12.4 / 12.7 | 29 / 26.4 / 27.1 | 2 / 1.9 / 1.9 | 4 / 2.7 / 2.8 |
| tess-best | 47 / 43.8 / 45.6 | 55 / 52.0 / 53.7 | 15 / 14.3 / 14.7 | 23 / 22.5 / 23.2 | 4 / 3.0 / 3.2 | 5 / 4.0 / 4.2 |
| manga-greedy | — / 147.7 / 175.2 | 229 / 177.0 / 223.6 | — / 156.9 / 197.8 | 118 / 174.3 / 232.9 | — / 127.6 / 190.5 | — / 118.1 / 148.6 |
| manga-beam | — / 221.5 / 260.8 | 196 / 223.9 / 271.4 | — / 204.2 / 268.0 | 201 / 204.8 / 248.3 | — / 138.5 / 182.5 | — / 140.8 / 190.6 |

Reference bar: the Windows hover round trip is ~141 ms including capture; the
research doc set ≤100 ms/crop as the "feels equivalent" target and ≥500 ms as
product-changing. meikiocr and both Tesseract variants sit comfortably inside
the budget; PP-OCRv5-mobile via the Python pipeline does not (≈250 ms p50 —
some of that is wrapper overhead, see the Python-vs-`ort` caveat; the det pass
dominates); manga-ocr sits between (~150–225 ms) and is size-insensitive
because every crop is squish-resized to 224×224.

## Engine construction and memory

| config | construction ms (min–max, fresh processes) | peak RSS MiB (full run) |
|---|---|---|
| meiki | 299–319 | 384 |
| ppocrv5 | 239–265 | 1242 |
| tess-fast | 99–112 | 142 |
| tess-best | 145–151 | 206 |
| manga-greedy | 377–387 | 710 |
| manga-beam | 374–375 | 780 |

All construction times are compatible with `OcrTextSource::new`'s
build-at-worker-start-and-reload-on-settings-change shape (R8).

## Masked variants — ΔCER vs unmasked 2× base (pp)

Cell format: **raw ΔCER (ΔCER after drop-filtering)** — the parenthesised
number removes predicted boxes that intersect the mask before scoring, i.e.
what survives chibipop's `layout.rs` clipped-word exclusion. manga-ocr has no
boxes, so no drop-filtered number exists (it would ship raw). The
outside-control row is a pipeline sanity check (mask never touches the crop).

**by position**

| position | meiki | ppocrv5 | tess-fast | tess-best | manga-greedy (whole) | manga-greedy (meiki-lines) | manga-beam (whole) | manga-beam (meiki-lines) |
|---|---|---|---|---|---|---|---|---|
| edge | +3.6 (+3.1) | +13.6 (+11.9) | +19.5 (+12.1) | +17.1 (+10.3) | +5.8 | +0.1 | +6.7 | −1.4 |
| interior | +33.8 (+11.6) | +13.8 (+1.9) | +63.9 (+45.7) | +67.7 (+50.1) | +125.8 | +19.6 | +79.5 | +17.8 |
| outside | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 |

**by fill** (edge+interior positions)

| fill | meiki | ppocrv5 | tess-fast | tess-best | manga-greedy (whole) | manga-greedy (meiki-lines) | manga-beam (whole) | manga-beam (meiki-lines) |
|---|---|---|---|---|---|---|---|---|
| black | +18.7 (+8.0) | +10.5 (+5.3) | +61.7 (+43.3) | +62.1 (+49.5) | +128.5 | +7.5 | +43.6 | +5.7 |
| gray | +17.1 (+6.8) | +11.3 (+5.3) | +64.5 (+53.2) | +67.3 (+54.6) | +44.6 | +9.7 | +41.3 | +7.6 |
| mean | +19.8 (+7.0) | +17.0 (+7.9) | +20.1 (+7.7) | +19.4 (+7.8) | +44.9 | +11.3 | +44.5 | +9.1 |
| white | +19.3 (+7.0) | +15.9 (+7.9) | +20.6 (+9.7) | +20.7 (+9.1) | +45.2 | +11.0 | +42.9 | +10.5 |

**by edge treatment** (edge+interior positions)

| edge | meiki | ppocrv5 | tess-fast | tess-best | manga-greedy (whole) | manga-greedy (meiki-lines) | manga-beam (whole) | manga-beam (meiki-lines) |
|---|---|---|---|---|---|---|---|---|
| feather | +18.2 (+6.6) | +14.0 (+6.2) | +42.7 (+26.4) | +44.7 (+27.9) | +90.6 | +9.5 | +44.8 | +7.6 |
| hard | +19.3 (+7.9) | +13.3 (+7.0) | +40.8 (+28.6) | +40.1 (+31.2) | +41.1 | +10.2 | +41.4 | +8.8 |

What the sweep says (data, for the mask-parameter decision):

- **Boundary hallucination is real but drop-filterable.** Every box engine
  emits 1–2.5 non-empty chunks *inside* the mask on interior positions;
  after dropping them (which production does anyway), meikiocr's interior
  penalty falls +33.8→+11.6 pp and PP-OCRv5's +13.8→**+1.9 pp**. The residual
  is misreads of legitimately visible text adjacent to the mask edge.
- **Fill color is the dominant knob for Tesseract and manga-ocr, irrelevant
  for meikiocr.** Black/gray fills destroy Tesseract (+43–55 pp *after*
  dropping — its binarization swallows the whole line; e.g. gray-interior on
  j1 read `前昌遇績`), while mean/white keep it at +7.7–9.7 pp. Black makes
  whole-crop manga-ocr confabulate fluent bridges across the mask
  (`学生は宿舎になっていて、一般をして` for GT `学生は宿舎…邪をひいて`;
  ~12 inserted chars/crop). meikiocr moves ≤1.2 pp across all four fills.
- **The 1 px feather is worthless on this evidence**: within noise for the box
  engines, and it *doubles* whole-crop manga-greedy damage (+41.1→+90.6 pp;
  the blended ring apparently reads as texture). Hard edges are fine.
- **Edge-position masks (popup partially outside the crop) are cheap for
  meikiocr (+3.1 pp after drop) and manga line-fed (≈0), moderate for
  PP-OCRv5 (+11.9 pp — its line quads get clipped and rectified rec degrades)
  and Tesseract (+10–12 pp).**

## Per-engine caveats observed

**meikiocr** — Vertical beta quality is visible and specific: the vertical
fixture loses its first glyph and trailing `。` at both scales
(`輩は猫である。名前はまだ無い`), plus `猫`→`描` at 2×; vertical hit-scan drops
to 75–81 % because the surviving char boxes shift. Horizontal is near-perfect
(the only 2× horizontal error is a spurious trailing `。` on one crop —
punctuation hallucination at a whole-glyph cut edge). 12 px mixed text loses
`il` from `Skill` (3.7 % small CER). Latency is flat ~21–22 ms p50 across every
crop size (fixed 960×544 detector input dominates), cold ≈ +5 ms, construction
~0.3 s, RSS 384 MiB.

**PP-OCRv5 mobile (RapidOCR)** — Best-in-test horizontal accuracy at 2×
(0.0 % CER everywhere except the smoke fixture) and functional per-char word
boxes (94–100 % hit-scan). Two sharp edges: (a) the detector finds nothing on
the sparse 3-glyph smoke fixture at either scale — isolated-short-text
detection failure, exactly the risk the research doc flagged for
cursor-at-crop-edge scenarios; (b) 1× accuracy is notably worse than 2×
(3.8 %/25 % vs 0 %/12.5 % horizontal/vertical) — it wants the upscale.
~250 ms p50 through the Python pipeline blows the 100 ms budget; a Rust `ort`
port would shed wrapper overhead, but the det+rec model cost on a 9800X3D
core is the floor to beat. RSS 1.24 GiB was the largest of any engine
(pipeline buffers; the models themselves are 21 MB). The `return_word_box`
path exists in this wrapper — whether `oar-ocr` reproduces it still needs
checking at implementation time.

**Tesseract 5 (jpn/jpn_vert, fast+best)** — On clean rendered horizontal text
the CER reputation problem does not bite: jpn-fast reads every horizontal crop
at 0–1.8 % CER at both scales, at 9–13 ms p50 with 142 MiB RSS. Everything
else has an asterisk. Geometry: LSTM symbol "boxes" are CTC-alignment blobs,
not glyph rects (measured example on j1: `学` w=50 followed by `生` w=4) →
33–62 % horizontal hit-scan; `jpn_vert` symbol boxes are fully degenerate
(x=0, w=0 for every glyph) so the harness fell back to word-level boxes for
vertical (50–56 % hit-scan) — `hit_scan` cannot be built on any of this.
Vertical: clean at 1× (0 % CER — best in test) but the 2× nearest-neighbour
upscale induces a hallucinated second column (`足馬は黄下` prefix, fast;
`ロロは#田0`, best) → 31–44 % CER on the production path. tess-best is
consistently *worse* than fast at 2× (7.0 % horizontal: `宿舎`→`宿人千`) while
costing 4× the latency. Masking: black/gray fills are catastrophic (above);
if Tesseract stays a candidate, the mask must be white/crop-mean.

**manga-ocr (ONNX, greedy + beam-4)** — The latency question is settled:
150–225 ms p50 warm on CPU (greedy≈175 ms, beam≈224 ms on 1000×200), ~0.38 s
construction, 710–780 MiB RSS; size-invariant (everything is squished to
224×224). Quality is bimodal. On normal-size single lines it matches or beats
everything (beam whole-crop: 0.0 % horizontal CER at 2×, including the
white-on-dark outlined block; smoke and vertical clean at 1×). But the 224×224
squish makes ≤16 px whole-crop text unreadable and the model **confabulates
fluent Japanese instead of failing** (s16 → `前の窓でも、保護者さんは`,
86–94 % CER) — the documented hallucination failure mode, reproduced. Feeding
meikiocr's line boxes fixes vertical (6.2 %) and halves small-glyph damage
(43–52 % — still unusable), at the cost of horizontal accuracy on long lines
(16.1 % — the 960-wide line crops lose detail in the squish; per-line feeds
are worse than the whole crop when the line fills the crop). Fullwidth
Latin/digit output (`ＬＩＮＫ`-style) is confirmed and handled by NFKC. Beam-4
improved CER by 1–5 pp over greedy on several slices for ~+30 % latency; no
geometry ever, so hit-scan is impossible without a detector in front.

## Benchmark limitations

- Rendered corpus uses one font family (Noto Sans CJK JP via fontconfig
  fallback — "Noto Sans JP"/"Yu Gothic UI" are not installed on this host) at
  26/16/12 px on flat backgrounds, plus one dark-panel variant. No font
  diversity, no textured game backgrounds, no furigana. The smoke fixture is
  the only Yu-Gothic-rendered input (Windows-generated).
- Small sample per slice (1–3 crops × 2 scales); CER/hit-scan differences
  under ~3 pp are within corpus noise. Latency numbers (100 iters, idle
  16-thread machine) are stable to ~±5 %.
- Python harness: same ONNX Runtime / libtesseract C libraries as the planned
  Rust integrations (`ort`, FFI), but per-call glue overhead is included;
  treat absolute latency as an upper bound, relative ordering as reliable.
  RapidOCR's number carries the most wrapper overhead.
- The Windows `chibipop read --time` control bar (~141 ms incl. capture) was
  not re-measured here (no Windows machine in this environment); the figure is
  the maintainer note from `src/text/ocr.rs` (2026-08-08).
- Masked GT excludes any character whose box touches the mask; engines are not
  penalized for the occluded text itself, only for what they do around it.

## Raw data

`tools/ocr-bench/results/*.json` — per-crop predictions, per-crop CER/edit
ops/hit-scan, latency samples' summary, cold runs (`cold.json`), environment
freeze (`env.txt`), auto-generated tables (`tables.md`). Corpus with
ground-truth overlays: `tools/ocr-bench/corpus/debug/`.
