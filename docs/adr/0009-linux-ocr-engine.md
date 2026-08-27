# meikiocr is the Linux OCR engine

`Windows.Media.Ocr` has no Linux equivalent, and chibipop's pipeline hard-requires
per-word geometry (`hit_scan`, orientation, tiling). Four candidates were benchmarked
against 152 ground-truthed fixture crops
([results](../research/ocr-benchmark-results.md), harness `tools/ocr-bench/`).

**Decision: meikiocr, ported to Rust over `ort`, is the sole Linux `OcrEngine`.**
It is the only candidate that clears every hard requirement: ~22 ms warm p50 flat
across crop sizes (5× under the ≤100 ms budget), per-character boxes (finer than
Windows' word rects), 94–100 % horizontal hit-scan, 0–3.5 % horizontal CER, correct
on the sparse smoke fixture, and the most mask-robust engine in the ADR-0008 sweep.
Its beta vertical mode ships as-is (12.5–18.8 % CER, 75–81 % hit-scan — degraded, not
broken) and is gated at that floor. The port mirrors meikiocr's ~500-line
numpy/OpenCV pre/post-processing around three ONNX sessions; no Python at runtime.
Windows keeps `Windows.Media.Ocr` untouched.

Rejected:

- **PP-OCRv5 mobile** — best horizontal CER at 2× and working char boxes, but a total
  detection miss on the sparse 3-glyph fixture (the cursor-at-crop-edge case) and
  ~250 ms p50 against a ≤100 ms budget.
- **Tesseract 5** — geometry is CTC-alignment blobs (33–62 % hit-scan; `jpn_vert`
  symbol boxes fully degenerate); fails the hit-scan contract outright.
- **manga-ocr** — no geometry by architecture; confabulates fluent Japanese on small
  text and black masks. Not shipped, not even as an optional quality mode (would add
  ~460 MB and a second inference path while anchoring still rode meiki's boxes).

## Quality gate

The fixture corpus is a standing regression gate, enforced as a Rust fixture test in
the Linux CI job (ADR-0007): the ported pipeline runs over the committed crops +
ground-truth JSON and asserts **horizontal CER ≤ 5 %, horizontal hit-scan ≥ 90 %,
vertical CER ≤ 20 %, vertical hit-scan ≥ 75 %**, and parity with the Python harness's
**1×** numbers within ±3 pp. Latency is asserted only as a generous ceiling in CI (runners
vary); the product bar stays warm p50 ≤ 100 ms per crop on developer hardware.
Models are fetched hash-pinned in CI and cached.

## Vertical mode and scaling (amended 2026-08-24)

- **The Linux adapter never upscales — native-resolution crops, any orientation.**
  Windows keeps its 2× `UPSCALE` for `Windows.Media.Ocr`; meiki measured 1× ≥ 2× on
  every benchmark slice (horizontal CER 1.8 vs 3.5 %, vertical 12.5 vs 18.8 %,
  small-glyph hit-scan 93.2 vs 90.9 %) because the fixed 960×544 detector letterbox
  undoes the upscale — worst on tall vertical crops. This meets the `src/text/ocr.rs`
  maintainer-note standard: upscale ships only on accuracy evidence, and the evidence
  says it loses. Re-measure before ever adding one back.
- **meiki's per-line `is_vertical` stays engine-internal** (rec/vrec routing only);
  core keeps deriving orientation geometrically via `orientation_of`, and
  `prefer_vertical` keeps its core-only meaning of transposing the capture rect.
  The `OcrEngine` trait and `OcrLine` are unchanged.
- **The vertical first-glyph/trailing-`。` drop is accepted beta quality** — no
  padding, offset, or retry mitigation (all unmeasured). The gate above prices it in.
  Re-run the vertical benchmark slice on any upstream vertical-model update. Docs
  state vertical is beta with the gate numbers; the UI stays silent.

## Distribution and licensing

- **Models bundled everywhere**: the default trio (det 15 + rec 19 + vrec 13 ≈ 47 MB
  ONNX) ships inside the release tarball and `chibipop-bin`. **Corrected in the
  2026-08-26 packaging pass:** the models are *committed to the repo*
  (`crates/chibipop-linux/models/meiki/`), so the source AUR package gets them
  from the tag archive and downloads nothing at build time; its `prepare()`
  re-checks the digests against `SHA256SUMS.txt` (the Hugging Face fetch this
  bullet used to describe never existed in the shipped layout). No first-run
  download path exists — the app stays offline-first.
- **ONNX Runtime**: release builds use `ort`'s pinned download-binaries and ship
  `libonnxruntime.so` beside the binary (rpath `$ORIGIN`) in the tarball and
  `chibipop-bin`; the source AUR package links the system `onnxruntime` instead.
  This is what ADR-0007's glibc-dynamic choice was preserving.
  **Superseded — see the 2026-08-26 addendum below: on linux-x64 there is no
  shared library to ship.**
- **Licenses**: meikiocr code Apache-2.0, models LGPL-3.0 (shipped as data files),
  ONNX Runtime MIT — all GPL-3.0-or-later-compatible. Redistribution of the LGPL
  weights is noted in the README and PKGBUILDs; no source obligations attach beyond
  pointing at upstream.

## ADR-0008 mask parameters, confirmed by measurement

**White fill, hard edge, no capture guard.** Fill color is irrelevant to meikiocr
(≤1.2 pp spread); white is safe across every benchmarked engine and costs nothing.
The 1 px feather bought nothing measurable. After production drop-filtering, interior
masks cost +11.6 pp — acceptable — so the Windows-style capture guard is not built;
it remains ADR-0008's named fallback on paper only.

## ONNX Runtime is statically linked, not bundled (amended 2026-08-26)

The bundling half of "Distribution and licensing" above rests on an assumption
that turned out to be false. The packaging work found out the only way
anyone was going to: by building the asset and looking in it.

**On linux-x64, `ort`'s pinned `download-binaries` prebuilt is a static
archive, not a shared object.** The default `bundled-onnxruntime` feature
therefore links ONNX Runtime *into* `chibipop`: nothing named
`libonnxruntime.so` is produced anywhere under `target/`, `ldd` on the release
binary lists only `libstdc++.so.6`, `libgcc_s.so.1`, `libm.so.6` and
`libc.so.6`, and the binary is ~62 MB because the runtime is inside it.

So:

- **No `libonnxruntime.so` is shipped and no rpath is set.** `$ORIGIN` was a
  mechanism for a problem this target does not have. The tarball carries one
  executable.
- **The intent is unchanged, and the tarball delivers it**: the asset works
  offline, on a machine with no ONNX Runtime installed, with no first-run
  download. Static linkage is a *stronger* form of that promise than a bundled
  `.so` — there is no library search order to get wrong and no way for a
  distro `libonnxruntime` to be picked up by accident.
- **The models are unaffected**: still bundled, still SHA-256-pinned to
  `SHA256SUMS.txt`, and now verified twice — by `scripts/package-linux.sh` as
  it stages the copies that go into the tarball, and by `models::verify` when
  the engine opens. A unit test asserts the checksum file and the compiled-in
  digests agree, because two lists of digests that disagree would pass the
  build gate and then refuse the models on the user's machine, which is the
  one failure mode bundling was supposed to remove.
- **The source AUR path is unaffected**: `--no-default-features --features
  system-onnxruntime` still dlopens the distro's `libonnxruntime.so`, which is
  the whole reason that feature exists.
- **ADR-0007's glibc-dynamic choice still stands, for its original reason and
  one more.** musl would still foreclose the pinned `ort` prebuilt; and glibc
  plus `libstdc++` is now the *entire* runtime floor, both of which come from
  the oldest ubuntu runner image the release workflow pins to.

Re-check this on any `ort` version bump. If upstream ships a shared prebuilt
for linux-x64 again, the tarball grows a file and this addendum is what needs
rewriting — not the intent above it.
