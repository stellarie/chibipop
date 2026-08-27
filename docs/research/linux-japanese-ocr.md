# Research: Japanese OCR engines on Linux

Research date: 2026-08-18. Support-status claims are dated inline where they may drift.

## Verdict

There is no drop-in replacement for `Windows.Media.Ocr`, but there are four
credible candidates, and two of them did not exist when this problem was last
surveyed by the wider community:

1. **meikiocr models via `ort`** — purpose-built for Japanese *video game*
   text (our exact domain), Apache-2.0 code + LGPL-3.0 models, ~47 MB total,
   ONNX, and — uniquely — returns **per-character bounding boxes**, which is
   strictly better geometry than the per-word rects chibipop gets from Windows
   today. Vertical support exists but is flagged beta by the author.
2. **PP-OCRv5 (mobile det+rec) via the `oar-ocr` crate** — Apache-2.0
   end-to-end, Japanese included in the default v5 recognition model, published
   per-line CPU recognition latency in the 5–21 ms range, word-level boxes
   available in the reference pipeline (with caveats), native Rust inference
   already maintained by a third party.
3. **Tesseract 5 `jpn`/`jpn_vert`** — the boring baseline: packaged by every
   distro, Apache-2.0, tiny models, word *and symbol* boxes. Its Japanese
   LSTM quality reputation is genuinely mediocre (documented upstream), so it
   is a control/fallback candidate, not a favourite.
4. **manga-ocr (ONNX export) via `ort`** — the community's quality reference
   for exactly this content (VN/manga/game text, vertical included), Apache-2.0
   weights, but it is a 460 MB seq2seq model that returns **text only, no
   geometry**, so it can only ever be a recognition stage behind someone
   else's detector — or a quality yardstick in the benchmark.

**Ruled out on licensing:** oneOCR (Microsoft Snipping Tool model extraction)
and Chrome Screen AI — both are proprietary, non-redistributable binaries that
users must extract from Microsoft/Google packages; neither can be a dependency
of a GPL-3.0-or-later app, and oneOCR's DLL is Windows-only anyway. Yomitoku
is CC BY-NC-SA 4.0 (non-commercial) — GPL-incompatible.
**Ruled out on capability:** EasyOCR (no vertical CJK, line-level boxes only,
dormant since 2024), docTR (no pretrained Japanese recognition model), ocrs
(Latin-only today), NDLOCR-Lite (CC BY 4.0, fine, but a page-digitisation
pipeline aimed at books, not 500×100 px screen crops — stretch candidate at
most).

**What the decision session must still decide** (after benchmarking, not from
reading): whether meikiocr's beta vertical mode is good enough to be the
primary engine, whether PP-OCRv5's word-box quirks (rectified boxes,
no isolated-single-char detection) break hit-scan, and whether manga-ocr's
latency on CPU is acceptable as an optional "quality mode". The benchmark
shortlist at the end of this document is designed to answer exactly those
three questions against the repo's fixtures.

---

## Baseline: what Windows.Media.Ocr gives chibipop today

The contract the Linux engine must match (from `src/text/ocr.rs` and the
WinRT API docs):

- `OcrEngine::RecognizeAsync(SoftwareBitmap)` → `OcrResult` → `Lines` →
  `Words`, where each `OcrWord` carries `Text` plus `BoundingRect`, "the
  position and size in pixels of the recognized word from the top left corner
  of image when TextAngle is 0" ([OcrWord docs](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrword)).
- chibipop's whole downstream pipeline consumes that geometry: `hit_scan`
  picks the word under the cursor from word rects, `orientation_of` infers
  vertical vs horizontal from word-rect layout, and tiling/continuation logic
  splits and orders words by their rects (`src/text/layout.rs`).
  **Word (or character) geometry is a hard requirement, not a nicety.**
- For Japanese, WinRT "words" are not linguistic words (Japanese has no
  spaces); they are recognizer-chosen chunks. chibipop already treats them as
  opaque positioned segments and does its own dictionary-driven segmentation,
  so a Linux engine that returns per-character boxes is fully sufficient —
  chunk granularity does not need to match Windows.
- Input shape: BGRA capture of a **500×100** (horizontal) or **100×500**
  (vertical) region around the cursor (`src/config.rs` defaults), upscaled
  **2×** before OCR (`UPSCALE` in `src/text/capture.rs`), i.e. the engine
  sees ~1000×200 px of clean rendered text.
- Latency budget: a maintainer note in `src/text/ocr.rs` (dated 2026-08-08)
  records a **~141 ms hover round trip** including capture, and that adding a
  second capture + second OCR pass at 4× pixels cost only ~36 ms more — so a
  single Windows OCR pass on this crop size is on the order of a few tens of
  ms. A Linux replacement should target **≤100 ms per crop on CPU** to feel
  equivalent; anything ≥500 ms changes the product.
- Mixed Japanese + alphanumeric in the same line must work (the
  `scan_alphanumeric` option resolves Latin-only words).
- Fully offline; language models installed locally.

## Requirements checklist

| # | Requirement | Source |
|---|-------------|--------|
| R1 | Word- or character-level bounding boxes | `hit_scan`, popup anchoring |
| R2 | Vertical (tategaki) recognition | `prefer_vertical`, VN/manga use case |
| R3 | Mixed JA + alphanumeric | `scan_alphanumeric` |
| R4 | Offline, local models | product constraint |
| R5 | GPL-3.0-or-later-compatible license (code *and* weights) | project license |
| R6 | Callable from Rust without a Python runtime (strongly preferred) | pure-Rust UI decision, packaging |
| R7 | Low per-call latency on ~1000×200 crops, CPU-first | ~141 ms Windows round trip |
| R8 | Modest startup cost (engine is constructed at worker start, reloadable on settings change) | `OcrTextSource::new` |

---

## Candidate matrix

| Candidate | License (code / models) | Model size | Boxes | Vertical JA | Rust integration | Status (2026-08) |
|---|---|---|---|---|---|---|
| meikiocr | Apache-2.0 / LGPL-3.0 | ~47 MB (det 15 + rec 19 + vrec 13) | **per-char** + line | yes (beta) | ONNX via `ort` (models are plain ONNX) | active |
| PaddleOCR PP-OCRv5 mobile | Apache-2.0 / Apache-2.0 | ~21 MB (det 4.7 + rec 16) | line quads; word boxes opt-in | yes (improved in v5) | native: `oar-ocr` crate (Apache-2.0), or ONNX via `ort` | very active |
| Tesseract 5 jpn/jpn_vert | Apache-2.0 / Apache-2.0 | 2.5–14 MB per lang | word + **symbol** | separate `jpn_vert` model | `leptess`/`tesseract`/`tesseract-rs` crates (C FFI) | mature/stable |
| manga-ocr (ONNX) | Apache-2.0 / Apache-2.0 | ~460 MB (343 enc + 117 dec) | **none** | yes (trained for it) | `manga-ocr-rs` crate (MIT) or own `ort` code | model frozen, ports active |
| oneOCR (extracted) | none / proprietary MS | n/a | line + word quads | yes | Windows DLL only | **disqualified: license + platform** |
| Chrome Screen AI | BSD callers / proprietary Google binary | n/a | yes | yes | none | **disqualified: license** |
| EasyOCR | Apache-2.0 | n/a | line quads only | **no** (vertical CJK unsupported) | Python only | dormant (last release 2024-09) |
| docTR | Apache-2.0 | n/a | word | n/a | Python (OnnxTR exists) | **disqualified: no JA rec model** |
| ocrs | MIT/Apache-2.0 | ~small | word + line | n/a | pure Rust (rten) | **disqualified: Latin-only** |
| NDLOCR-Lite | CC BY 4.0 | several ONNX models | line | yes | ONNX via `ort` (feasible) | active; doc/book domain |
| Yomitoku | CC BY-NC-SA 4.0 | n/a | yes | yes | n/a | **disqualified: NC license** |

---

## Detailed findings

### 1. Tesseract 5 (`jpn` + `jpn_vert`)

- **License:** Apache-2.0 for the engine
  ([tesseract-ocr/tesseract](https://github.com/tesseract-ocr/tesseract)) and
  for all three official traineddata repos (LICENSE in
  [tessdata_fast](https://github.com/tesseract-ocr/tessdata_fast) is the
  Apache-2.0 text). GPLv3-compatible per the
  [FSF license list](https://www.gnu.org/licenses/license-list.html#apache2).
- **Models:** three official sets —
  [tessdata_fast](https://github.com/tesseract-ocr/tessdata_fast) (integer
  LSTM, what distros ship), [tessdata_best](https://github.com/tesseract-ocr/tessdata_best)
  (float LSTM, most accurate), legacy tessdata
  ([Data Files doc](https://tesseract-ocr.github.io/tessdoc/Data-Files.html)).
  Verified sizes via the GitHub API (2026-08-18): `jpn` fast **2.47 MB**,
  `jpn_vert` fast **3.04 MB**, `jpn_vert` best **14.33 MB**. Vertical text is
  a *separate* traineddata (`jpn_vert`), used with `--psm 5`; chibipop already
  decides orientation per pass, so driving two engines is structurally easy.
- **Geometry:** the best in class on paper. TSV/hOCR output and the C API's
  `ResultIterator` expose boxes at block/line/word/**symbol** level
  (`RIL_SYMBOL`), so per-character rects are available — for Japanese the
  "word" level is meaningless (arbitrary chunks), symbol level is what we'd
  map to `OcrWord`.
- **Japanese quality reputation (the known problem):** upstream issues
  document that the official Japanese LSTM model was trained on few fonts
  ([#3138](https://github.com/tesseract-ocr/tesseract/issues/3138), 2020,
  still open), that LSTM vertical Japanese is markedly worse than horizontal
  ([#627](https://github.com/tesseract-ocr/tesseract/issues/627)), that
  vertical best-model runs can reuse characters across "words"
  ([#1117](https://github.com/tesseract-ocr/tesseract/issues/1117)), and that
  spurious spaces are inserted between CJK characters in most output formats
  ([#2702](https://github.com/tesseract-ocr/tesseract/issues/2702),
  [#3645](https://github.com/tesseract-ocr/tesseract/issues/3645);
  workaround `-c preserve_interword_spaces=1` for text output). chibipop's
  `normalise()` already scrubs some OCR artifacts, and clean 2×-upscaled
  rendered text is Tesseract's best case — but expectations should be
  "adequate", not "Windows-parity". No official latency figures exist;
  small-crop LSTM passes are commonly tens of ms, to be measured.
- **Rust integration:** in-process C FFI. Crates (checked on crates.io
  2026-08-18): [`leptess`](https://crates.io/crates/leptess) 0.14.0 (MIT,
  last release 2023-02 — dormant), [`tesseract`](https://crates.io/crates/tesseract)
  0.15.2 (2025-04), [`tesseract-rs`](https://crates.io/crates/tesseract-rs)
  0.4.0 (2026-07, offers built-in compilation of tesseract+leptonica, which
  solves the system-dependency problem for release builds),
  [`rusty-tesseract`](https://crates.io/crates/rusty-tesseract) (subprocess
  wrapper — avoid; per-call process spawn is the wrong shape for hover
  latency). System packages exist on every distro as a fallback integration.
- **Startup:** engine + 3 MB model init is historically fast (<100 ms class);
  to be measured.

### 2. manga-ocr (and its ONNX/Rust ports)

- **What it is:** a Transformers VisionEncoderDecoder (ViT encoder + BERT
  decoder) trained by kha-white specifically for Japanese manga; explicitly
  supports "both vertical and horizontal text", furigana-polluted text, text
  over images, many fonts, low-res input, and multi-line crops in one pass;
  the author states it "should do a decent job" on video games and novels
  ([README](https://github.com/kha-white/manga-ocr), Apache-2.0).
- **License:** Apache-2.0 for code and weights
  ([kha-white/manga-ocr-base on HF](https://huggingface.co/kha-white/manga-ocr-base),
  license tag `apache-2.0`). GPL-compatible.
- **Size / shape:** PyTorch checkpoint 444 MB. ONNX export is a solved
  problem: upstream issue [#45](https://github.com/kha-white/manga-ocr/issues/45),
  `optimum-cli --task vision2seq-lm`, and a canonical pre-exported repo
  [mayocream/manga-ocr-onnx](https://huggingface.co/mayocream/manga-ocr-onnx)
  (Apache-2.0; encoder 343 MB + decoder 117 MB, verified via HF API
  2026-08-18). A third-party slimmed/quantised export exists
  ([l0wgear/manga-ocr-2025-onnx](https://huggingface.co/l0wgear/manga-ocr-2025-onnx),
  encoder 22 MB) but carries **no license tag** — do not use without
  clarification.
- **Rust path exists today:** [`manga-ocr-rs`](https://github.com/CodeMonkeyNinja/manga-ocr-rs)
  (MIT, crates.io v1.0.0, 2026-07) runs the mayocream export via ONNX
  Runtime: 224×224 squish-resize input, beam search k=4, autoregressive
  decode up to 300 steps. Its README is also the only published latency
  datapoint: ~1.8–2.0 s per crop **in debug builds** ("release significantly
  faster" — unquantified), and it documents the model's failure mode of
  confidently hallucinating text on decorative/empty crops (mitigated by a
  confidence+truncation threshold). It is used in production by the Lenzu
  Linux overlay tool.
- **The blocker: no geometry.** The model emits a text string per crop —
  no line, word, or character boxes, by architecture. Ecosystem tools pair it
  with a detector ([comic-text-detector](https://github.com/dmMaze/comic-text-detector),
  GPL-3.0 — license-compatible but a second ~large model and Python-trained),
  as mokuro, owocr, and YomiNinja do. Even detector line boxes don't give the
  *within-line* character positions `hit_scan` needs; positions would have to
  be synthesized (uniform division is unreliable for mixed-width JA+Latin
  lines). Realistic roles for chibipop: (a) recognition-only stage over
  another engine's line boxes with synthesized char positions, or (b) a
  benchmark quality ceiling.
- **Also note:** output uses full-width forms for Latin/digits (README
  example: `ＬＩＮＫ！`), so R3 requires NFKC-style normalization; startup
  means loading ~460 MB into RAM.

### 3. PaddleOCR PP-OCRv5 (and the Rust wrappers around it)

- **License:** Apache-2.0, code and released models
  ([PaddlePaddle/PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR)).
- **Japanese support:** PP-OCRv5's *default* recognition model covers
  "Simplified Chinese, Chinese Pinyin, Traditional Chinese, English, and
  Japanese" in one model, with explicitly upgraded vertical-text capability
  ([PP-OCRv5 introduction](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/algorithm/PP-OCRv5/PP-OCRv5.html)).
  Internal metrics there: Japanese rec accuracy 0.757 (mobile) / 0.737
  (server); "Vertical Text" 0.809 / 0.931. `lang=japan` is a first-class tag
  ([multilingual doc](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/algorithm/PP-OCRv5/PP-OCRv5_multi_languages.html));
  the old dedicated `japan_PP-OCRv3_mobile_rec` (9.8 MB, acc 45.7 on their
  set) is superseded by the default v5 model for our purposes.
- **Sizes / latency (official module docs, fetched 2026-08-18):**
  `PP-OCRv5_mobile_det` 4.7 MB, CPU 57.8 / 28.2 ms per image (normal /
  high-perf mode); `PP-OCRv5_mobile_rec` 16 MB, CPU 21.2 / 5.3 ms per text
  line; server det 84.3 MB, server rec 81 MB, CPU 31.2 ms/line
  ([text_detection](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/module_usage/text_detection.html),
  [text_recognition](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/module_usage/text_recognition.html)).
  Their full-page CPU pipeline average (200 doc/general images, Xeon 6271C)
  is 1.75 s for v5 mobile — but that is page-scale input at det side 736; a
  1000×200 crop is a fraction of that work. Detection input can be capped
  (`max`/640) for speed. Needs measuring on our crops.
- **Geometry:** detection returns per-text-line quads. Word-level boxes are
  an opt-in pipeline feature, `return_word_box` (bool, default false, in the
  [3.x OCR pipeline docs](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/pipeline_usage/OCR.html));
  for CJK content the word boxes are effectively per-character (see e.g.
  [discussion #14570](https://github.com/PaddlePaddle/PaddleOCR/discussions/14570),
  which calls them "character boxes" for CJK) — derived from CTC column
  alignment, so treat as approximate. Two caveats from upstream threads:
  word boxes are computed on the *rectified* line, and re-projection to
  original image coordinates is lossy for rotated lines
  ([discussion #17150](https://github.com/PaddlePaddle/PaddleOCR/discussions/17150));
  and the detector does not reliably find a lone isolated character
  ([discussion #7053](https://github.com/PaddlePaddle/PaddleOCR/discussions/7053))
  — relevant when the cursor hovers a single glyph at a crop edge.
  Whether the box path survives in a Rust reimplementation must be checked
  per wrapper (oar-ocr's coverage of `return_word_box` semantics is a
  benchmark item).
- **Rust integration — no Python needed:** paddle-inference has no sane Rust
  story, but the models convert to ONNX (paddle2onnx) and a healthy Rust
  ecosystem already runs them:
  [`oar-ocr`](https://github.com/GreatV/oar-ocr) (Apache-2.0, crates.io
  v0.9.2, pushed 2026-08-18; "end-to-end text detection and recognition with
  PP-OCR models, including PP-OCRv6"; available rec checkpoints explicitly
  include Japanese), and [retto](https://github.com/NekoImageLand/retto)
  (Apache-2.0, PaddleOCR on desktop/WASM via `ort`).
  [RapidOCR](https://github.com/RapidAI/RapidOCR) (Apache-2.0) is the
  canonical ONNX conversion/distribution of the Paddle models if we roll our
  own `ort` pipeline. YomiNinja (GPL-3.0 hover-dictionary app, closest
  ecosystem precedent) ships PaddleOCR as its local engine via a C++ gRPC
  service ([YomiNinja README](https://github.com/matt-m-o/YomiNinja)).

### 4. meikiocr — the domain-exact newcomer

- **What it is:** "high-speed, high-accuracy, local ocr for japanese video
  games" ([rtr46/meikiocr](https://github.com/rtr46/meikiocr), Apache-2.0):
  a two-stage ONNX pipeline — a text-line detector and a per-character CTC
  recognizer — trained specifically on rendered Japanese game text. The
  author claims it "significantly outperforms general-purpose ocr tools like
  paddleocr or easyocr on this specific domain" and publishes
  accuracy/latency pareto charts (relative, no absolute ms tables). owocr
  (GPL-3.0, the multi-engine OCR aggregator) recommends it as *the* local
  engine on Linux, "comparable to OneOCR in accuracy and CPU latency"
  ([owocr README](https://github.com/AuroraWright/owocr)).
- **License:** code Apache-2.0; **models LGPL-3.0** (HF license tags on
  [meiki.text.detect.v0](https://huggingface.co/rtr46/meiki.text.detect.v0)
  and [meiki.txt.recognition.v0](https://huggingface.co/rtr46/meiki.txt.recognition.v0),
  checked 2026-08-18). Both GPL-3.0-or-later-compatible. LGPL on weights is
  unusual; effect is negligible for us (we'd ship them as data files).
- **Sizes (HF API, 2026-08-18):** detector 960×544 15 MB (also 320×192
  14 MB, tiny 11 MB, small 42 MB variants), horizontal rec 960×32 19 MB,
  vertical rec 32×480 13 MB → **~47 MB** for the default trio.
- **Geometry — the standout:** `run_ocr` returns, per detected line, `text`,
  `is_vertical`, and **`chars`: per-character bounding boxes with confidence**
  (verified in [`meikiocr/ocr.py`](https://github.com/rtr46/meikiocr/blob/main/meikiocr/ocr.py)).
  That maps directly onto chibipop's `OcrLine`/`OcrWord` with one char per
  word — better anchoring granularity than Windows gives us.
- **Vertical:** dedicated vertical recognition model, line direction chosen
  by box aspect ratio; README flags vertical accuracy as **beta**.
- **Limits:** detector capped at 64 text boxes and 48 chars per line
  (irrelevant at our crop size); fixed detector input 960×544 (a 2×-upscaled
  1000×200 crop letterboxes into it naturally).
- **Integration:** the pipeline is Python but thin — ~500 lines of
  numpy/OpenCV pre/post-processing around three `onnxruntime` sessions.
  The models are plain ONNX; a Rust port over `ort` + `image` is a
  contained, well-specified task (mirror `_preprocess_for_detection`,
  batching, and the char postprocess). No Python at runtime.
- **Risk:** single-maintainer project (v0 models, 89 stars); we would be
  taking on the port and potential model-freeze risk. Mitigated by Apache/LGPL
  licensing (we can vendor everything) and by the fact that it is already
  load-bearing in owocr.

### 5. oneOCR (Windows Snipping Tool model) — evaluated, rejected

- The extraction route: copy `oneocr.dll`, `oneocr.onemodel`,
  `onnxruntime.dll` out of the Microsoft Store Snipping Tool MSIX (via
  store.rg-adguard.net) into a config dir; a Python ctypes wrapper then
  yields lines + **word quads + confidences**
  ([AuroraWright/oneocr](https://github.com/AuroraWright/oneocr)). Quality
  reputation is top-tier for Japanese.
- **Why rejected:** (a) the model and DLL are proprietary Microsoft
  components with **no license permitting extraction or redistribution** —
  a GPL project cannot depend on them, and we could never ship them;
  (b) `oneocr.dll` is a Windows PE — owocr lists the engine as "Windows
  10/11 only" and its Linux story is literally "run a server in a Windows
  VM" ([owocr README](https://github.com/AuroraWright/owocr));
  (c) the wrapper repo itself declares no license. Not a candidate; noted
  here because ecosystem tools keep mentioning it and its accuracy is the
  bar meikiocr measures itself against.

### 6. Chrome Screen AI — same category, rejected

- Chromium's on-device OCR ("Screen AI") is "developed within Google's
  internal source code repository (google3)" and delivered as a binary via
  the component updater for Linux/Mac/Windows
  ([Chromium services/screen_ai README](https://chromium.googlesource.com/chromium/src/+/main/services/screen_ai/README.md)).
  owocr downloads it from chrome-infra-packages and rates it "possibly the
  best local engine to date". The library is proprietary; no redistribution
  or standalone-use grant exists. Rejected for the same reasons as oneOCR,
  despite running natively on Linux.

### 7. EasyOCR — rejected on capability

- Apache-2.0, PyTorch (CRAFT detector + CRNN recognizer), `ja` supported
  ([JaidedAI/EasyOCR](https://github.com/JaidedAI/EasyOCR)). Output is
  **line-level quads only** (README example format) — no word/char geometry.
- **Vertical Japanese is unsupported:** the project's own release note says
  vertical text support "is for rotated text, not to be confused with
  vertical Chinese or Japanese text"; requests remain open since 2020
  ([#227](https://github.com/JaidedAI/EasyOCR/issues/227),
  [#686](https://github.com/JaidedAI/EasyOCR/issues/686)).
- Last release 1.7.2, 2024-09-24; effectively dormant. Python-only. Fails
  R1, R2, R6.

### 8. docTR — rejected on capability

- Apache-2.0, detection+recognition with word boxes
  ([mindee/doctr](https://github.com/mindee/doctr)). Detection is
  script-agnostic, but **there is no pretrained Japanese recognition model**;
  adding Japanese means training your own vocab/model
  ([discussion #1468](https://github.com/mindee/doctr/discussions/1468),
  [issue #563](https://github.com/mindee/doctr/issues/563), open since 2021).
  Fails R2/R3 out of the box; a training project is out of scope.

### 9. ocrs / pure-Rust engines — rejected today, watch list

- [ocrs](https://github.com/robertknight/ocrs) (MIT/Apache-2.0, pure Rust on
  the [rten](https://github.com/robertknight/rten) engine) is exactly the
  integration shape we want, but "currently recognizes the Latin alphabet
  only"; more languages are an open plan
  ([issue #8](https://github.com/robertknight/ocrs/issues/8)). Training a
  Japanese model via ocrs-models is a research project, not a port. Re-check
  at implementation time.

### 10. NDLOCR-Lite — capable but wrong domain

- National Diet Library's lightweight OCR
  ([ndl-lab/ndlocr-lite](https://github.com/ndl-lab/ndlocr-lite), **CC BY
  4.0**, which the FSF lists as GPLv3-compatible
  ([license list](https://www.gnu.org/licenses/license-list.html#ccby))).
  DEIMv2 layout detector (1024×1024) + PARSeq recognizers, all ONNX,
  CPU-oriented, horizontal and vertical, actively developed (v1.2, 2026-04).
- Built for digitised books/magazines: full-page layout analysis + reading
  order, detector at page scale. Plausible via `ort`, but heavier and
  domain-mismatched for 500×100 UI crops; recognition is per-line text
  (PARSeq) without per-char boxes in its XML/JSON output. Keep as a stretch
  candidate only if the shortlist disappoints on kanji coverage of old fonts
  (not our use case).

### 11. Yomitoku — rejected on license

- Modern Japanese document-OCR package; code **and weights** are CC BY-NC-SA
  4.0 with a paid commercial edition
  ([README](https://github.com/kotaro-kinoshita/yomitoku)). Non-commercial
  restriction makes it non-free and GPL-incompatible. Not a candidate.

---

## Rust inference substrate

- **[`ort`](https://github.com/pykeio/ort)** — the default choice for every
  ONNX candidate above: dual MIT/Apache-2.0, currently 2.0.0-rc.13
  (2026-07-28), wraps ONNX Runtime 1.28 (MIT), ~16 M downloads; despite the
  "rc" tag it is the production standard (oar-ocr, retto, manga-ocr-rs, and
  much of the ecosystem sit on it). It links the ONNX Runtime C library
  (downloaded or system); `ort` also offers pure-Rust backends (tract,
  candle) at reduced operator coverage if we ever need to drop the C
  dependency.
- All-Rust alternatives if ONNX Runtime linkage is unwanted:
  [tract](https://github.com/sonos/tract) (MIT/Apache-2.0) and
  [rten](https://github.com/robertknight/rten) (Apache-2.0) — slower, and
  operator coverage must be validated per model (notably manga-ocr's
  encoder/decoder graphs).
- **Sidecar-process pattern** (YomiNinja's gRPC service, owocr's servers) is
  the fallback for Python-only engines; it fails R6's spirit (packaging a
  Python runtime) and adds IPC latency, so it is only relevant if a
  Python-only engine wins the benchmark decisively — none is expected to.

## License compatibility with GPL-3.0-or-later

| Component | License | GPLv3+ compatible? |
|---|---|---|
| Tesseract + tessdata(_fast/_best) | Apache-2.0 | yes ([FSF](https://www.gnu.org/licenses/license-list.html#apache2)) |
| manga-ocr code + weights; mayocream ONNX export | Apache-2.0 | yes |
| PaddleOCR code + models; oar-ocr; retto; RapidOCR | Apache-2.0 | yes |
| meikiocr code | Apache-2.0 | yes |
| meikiocr models | LGPL-3.0 | yes |
| ort / ONNX Runtime | MIT+Apache-2.0 / MIT | yes |
| manga-ocr-rs, leptess | MIT | yes |
| comic-text-detector, owocr (reference only) | GPL-3.0 | yes |
| NDLOCR-Lite | CC BY 4.0 | yes, GPLv3 only ([FSF](https://www.gnu.org/licenses/license-list.html#ccby)) |
| oneOCR model/DLL, Chrome Screen AI binary | proprietary, no grant | **no** |
| Yomitoku | CC BY-NC-SA 4.0 | **no** (non-commercial) |
| l0wgear/manga-ocr-2025-onnx | none declared | **no** (until clarified) |

## Benchmark shortlist (next step — no winner picked here)

Run all four against the repo's fixtures: `tests/fixtures/japanese_bgra.bin`
(400×120 BGRA, exact production pixel format) and crops rendered from
`docs/fixtures/ocr-corpus.html` at 500×100 / 100×500, at 1× and 2× upscale,
plus a small-glyph set (≤16 px) and mixed JA+alphanumeric lines. Windows
numbers from `chibipop read --time` are the control bar (~141 ms round trip
incl. capture; word rects).

| # | Configuration | Why it's on the list |
|---|---|---|
| 1 | meikiocr det(960×544)+rec+vrec via `ort` (port the ~500-line pipeline) | Only candidate with per-char boxes; domain-exact training; 47 MB; must validate beta vertical + small-glyph behaviour |
| 2 | PP-OCRv5 mobile det+rec via `oar-ocr` (fallback: own `ort` pipeline over RapidOCR ONNX exports) | Best-documented latency (rec 5–21 ms/line CPU); JA in default model; must validate word-box fidelity for hit-scan and isolated-char detection |
| 3 | Tesseract 5.x, `jpn` + `jpn_vert`, tessdata_fast and tessdata_best, `preserve_interword_spaces=1`, symbol-level boxes, via `tesseract-rs` (bundled build) | Zero-risk baseline; quantifies whether its documented JA weaknesses actually bite on clean 2× rendered text |
| 4 | manga-ocr (mayocream ONNX, greedy *and* beam k=4) via `ort`, fed (a) whole crops, (b) meikiocr line boxes | Quality ceiling reference; determines if a "slow accurate mode" is viable and how bad CPU latency really is in release builds |

Measurements per configuration: CER after `normalise()`-equivalent cleanup;
hit-scan success rate (simulated cursor over each ground-truth word — this is
the geometry test that matters, not raw IoU); per-crop latency cold and warm
(p50/p95 over ≥100 iterations, CPU-only); engine construction time; RSS.
Decision inputs for the session: pass/fail on vertical fixtures for #1, box
fidelity for #2, whether #3 clears a usable CER floor, and whether #4's warm
latency lands under ~500 ms.

## Open risks and disagreements between sources

- **meikiocr's claims are self-published** (pareto charts without absolute
  numbers); owocr's independent "comparable to OneOCR" endorsement is the
  only second source. The benchmark is the arbiter.
- **PaddleOCR word boxes:** official docs only say the flag exists; the
  per-character CJK behaviour and its rectification caveats come from issue
  threads ([#14570](https://github.com/PaddlePaddle/PaddleOCR/discussions/14570),
  [#17150](https://github.com/PaddlePaddle/PaddleOCR/discussions/17150)).
  Whether `oar-ocr` reproduces the word-box path at all is unverified —
  check before benchmarking, else compute char splits ourselves from CTC
  columns.
- **manga-ocr latency:** no authoritative CPU figures exist anywhere;
  the only published numbers are debug-build (~2 s/crop). Do not size the
  architecture around it until measured.
- **Tesseract:** community consensus (issues above) says JA quality is weak,
  but those reports are dominated by scans/photos and pre-5.x versions; clean
  upscaled UI text is untested territory in the public record.
- **Model licensing drift:** meikiocr models are v0 and could relicense;
  vendor copies of exact model files (hashes) once chosen. l0wgear's slim
  manga-ocr export is unusable until it grows a license.

## Sources

- Windows baseline: [OcrWord class docs](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrword) (Microsoft Learn); repo: `src/text/ocr.rs`, `src/text/layout.rs`, `src/config.rs`, `tests/fixtures/README.md`
- Tesseract: [Data Files](https://tesseract-ocr.github.io/tessdoc/Data-Files.html) · [tessdata_fast](https://github.com/tesseract-ocr/tessdata_fast) · [tessdata_best](https://github.com/tesseract-ocr/tessdata_best) · issues [#627](https://github.com/tesseract-ocr/tesseract/issues/627), [#988](https://github.com/tesseract-ocr/tesseract/issues/988), [#1117](https://github.com/tesseract-ocr/tesseract/issues/1117), [#2702](https://github.com/tesseract-ocr/tesseract/issues/2702), [#3138](https://github.com/tesseract-ocr/tesseract/issues/3138), [#3645](https://github.com/tesseract-ocr/tesseract/issues/3645) · crates: [leptess](https://crates.io/crates/leptess), [tesseract](https://crates.io/crates/tesseract), [tesseract-rs](https://crates.io/crates/tesseract-rs), [rusty-tesseract](https://crates.io/crates/rusty-tesseract)
- manga-ocr: [kha-white/manga-ocr](https://github.com/kha-white/manga-ocr) · [manga-ocr-base (HF)](https://huggingface.co/kha-white/manga-ocr-base) · [issue #45](https://github.com/kha-white/manga-ocr/issues/45) · [mayocream/manga-ocr-onnx (HF)](https://huggingface.co/mayocream/manga-ocr-onnx) · [manga-ocr-rs](https://github.com/CodeMonkeyNinja/manga-ocr-rs) · [manga-ocr-torchless](https://github.com/liksunrice/manga-ocr-torchless) · [l0wgear/manga-ocr-2025-onnx (HF)](https://huggingface.co/l0wgear/manga-ocr-2025-onnx) · [comic-text-detector](https://github.com/dmMaze/comic-text-detector)
- PaddleOCR: [PP-OCRv5 intro](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/algorithm/PP-OCRv5/PP-OCRv5.html) · [PP-OCRv5 multilingual](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/algorithm/PP-OCRv5/PP-OCRv5_multi_languages.html) · [text detection module](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/module_usage/text_detection.html) · [text recognition module](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/module_usage/text_recognition.html) · [OCR pipeline (return_word_box)](https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/pipeline_usage/OCR.html) · discussions [#7053](https://github.com/PaddlePaddle/PaddleOCR/discussions/7053), [#14570](https://github.com/PaddlePaddle/PaddleOCR/discussions/14570), [#17150](https://github.com/PaddlePaddle/PaddleOCR/discussions/17150) · [oar-ocr](https://github.com/GreatV/oar-ocr) · [retto](https://github.com/NekoImageLand/retto) · [RapidOCR](https://github.com/RapidAI/RapidOCR)
- meikiocr: [rtr46/meikiocr](https://github.com/rtr46/meikiocr) · [ocr.py](https://github.com/rtr46/meikiocr/blob/main/meikiocr/ocr.py) · [meiki.text.detect.v0 (HF)](https://huggingface.co/rtr46/meiki.text.detect.v0) · [meiki.txt.recognition.v0 (HF)](https://huggingface.co/rtr46/meiki.txt.recognition.v0)
- oneOCR / Screen AI: [AuroraWright/oneocr](https://github.com/AuroraWright/oneocr) · [Chromium screen_ai README](https://chromium.googlesource.com/chromium/src/+/main/services/screen_ai/README.md)
- EasyOCR: [JaidedAI/EasyOCR](https://github.com/JaidedAI/EasyOCR) · issues [#227](https://github.com/JaidedAI/EasyOCR/issues/227), [#686](https://github.com/JaidedAI/EasyOCR/issues/686)
- docTR: [mindee/doctr](https://github.com/mindee/doctr) · [discussion #1468](https://github.com/mindee/doctr/discussions/1468) · [issue #563](https://github.com/mindee/doctr/issues/563)
- ocrs: [robertknight/ocrs](https://github.com/robertknight/ocrs) · [issue #8](https://github.com/robertknight/ocrs/issues/8) · [rten](https://github.com/robertknight/rten)
- NDLOCR-Lite: [ndl-lab/ndlocr-lite](https://github.com/ndl-lab/ndlocr-lite)
- Yomitoku: [kotaro-kinoshita/yomitoku](https://github.com/kotaro-kinoshita/yomitoku)
- Ecosystem precedents: [owocr](https://github.com/AuroraWright/owocr) (GPL-3.0) · [YomiNinja](https://github.com/matt-m-o/YomiNinja) (GPL-3.0) · [mokuro](https://github.com/kha-white/mokuro)
- Rust inference: [pykeio/ort](https://github.com/pykeio/ort) · [ort on crates.io](https://crates.io/crates/ort) · [tract](https://github.com/sonos/tract)
- License compatibility: [FSF license list — Apache-2.0](https://www.gnu.org/licenses/license-list.html#apache2), [CC BY 4.0](https://www.gnu.org/licenses/license-list.html#ccby)
