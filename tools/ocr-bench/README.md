# OCR candidate benchmark

Self-contained harness behind
[docs/research/ocr-benchmark-results.md](../../docs/research/ocr-benchmark-results.md).
It measures the four engines shortlisted in
[docs/research/linux-japanese-ocr.md](../../docs/research/linux-japanese-ocr.md)
against ground-truthed crops of the repo's OCR fixtures, including the
ADR-0008 masked-popup variants.

Everything this directory downloads or generates stays **inside this
directory** (venv, micromamba prefix, model files, caches, corpus, results).
Nothing is installed on the host; no sudo is used.

## Reproduce

```sh
cd tools/ocr-bench
./setup.sh                              # venv + user-space tesseract + models (~620 MB)
source env.sh                           # pins HF/pip/mamba caches inside this dir
.venv/bin/python -m bench.gen_corpus    # render + slice the ground-truthed corpus
.venv/bin/python -m bench.run_all       # all engines, sequential; ~10-20 min CPU
```

Outputs land in `results/`: one JSON per engine configuration, `cold.json`
(cold-start passes), `env.txt` (version provenance), `tables.md` (the
aggregated markdown tables embedded in the results doc). Re-aggregate tables
alone with `.venv/bin/python -m bench.report`. Model hashes are recorded in
`models/SHA256SUMS.txt`.

Host prerequisites (any Linux distro, x86_64 or aarch64): `python3` ≥ 3.10
(3.14 works; onnxruntime ships cp314 wheels), a Chromium-based browser for
corpus rendering (`chromium`, `chromium-browser`, `google-chrome`, … are
auto-detected; override with `OCR_BENCH_CHROMIUM=/path/to/browser`), `curl`,
`tar` with bzip2 support, and a CJK font (Noto Sans CJK JP or equivalent —
the corpus page falls back through `sans-serif` for JP glyphs).

## Layout

- `env.sh` — cache/env pinning sourced by everything.
- `setup.sh` — venv, micromamba+tesseract (conda-forge, user-space), model
  downloads with SHA-256 recording.
- `render/wrapper.html` — embeds `docs/fixtures/ocr-corpus.html` untouched in
  an iframe, adds two ≤16 px small-glyph lines, and reports per-character DOM
  geometry (`Range.getBoundingClientRect`) through `document.title`.
- `bench/gen_corpus.py` — headless-chromium render, crop slicing
  (500×100 / 100×500 with whole-glyph trimming), the
  `tests/fixtures/japanese_bgra.bin` smoke case (BGRA decoded exactly as
  production produces it; char boxes recovered by ink projection), 2×
  nearest-neighbour upscales mirroring `src/text/capture.rs upscale_by`, and
  the ADR-0008 mask sweep (position × fill × edge).
- `bench/eng_*.py` — engine adapters (meikiocr; PP-OCRv5 mobile via RapidOCR
  with `return_word_box`; Tesseract 5 via its C API over ctypes, symbol-level
  boxes; manga-ocr ONNX with greedy and batched beam-4 decoding).
- `bench/run_one.py` — one engine config per process: accuracy over all 152
  crops, warm latency (≥100 iters per crop size), construction time, peak RSS.
- `bench/run_all.py` — sequential driver + cold-start passes + aggregation.
- `bench/report.py` — `results/*.json` → `results/tables.md`.

## Host changes

None. No package-manager transactions, no systemd units, no writes outside this
directory (tesseract 5.5.3 comes from conda-forge into `.mamba/`; traineddata
files live in `models/tessdata/` via `TESSDATA_PREFIX`).

Note: constructing the RapidOCR pipeline downloads its default textline-cls
model into `.venv/.../rapidocr/models/` even though classification is disabled
at call time — still inside this directory.

## Cleanup

```sh
rm -rf tools/ocr-bench/.venv tools/ocr-bench/models tools/ocr-bench/.mamba \
       tools/ocr-bench/.cache tools/ocr-bench/bin tools/ocr-bench/corpus \
       tools/ocr-bench/results
```

(Everything listed is already gitignored via the local `.gitignore`.)
