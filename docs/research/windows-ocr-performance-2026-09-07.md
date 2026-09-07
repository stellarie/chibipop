# Windows OCR performance verification

The follow-ups below record later delivery hashes and the current key-capture UI.

Verified: 2026-09-07. Baseline: `1a0ace6`. Work branch: `feat/ocr-hot-path-performance`.

## Diagnosis

The earlier diagnosis identified inference costs and excessive action logs. It did not compare the configured engines on identical pixels.
It also stopped before dictionary popup paint. This round adds fixed-image comparisons, repeat-read measurements, real popup paint, and shortcut validation.

- Windows capture always reports `unchanged=false`. The earlier reuse guard therefore prevented Windows OCR reuse.
- Every DXGI crop created a staging texture. Persistent DXGI failures also retried initialization on every capture.
- Nearest-neighbor expansion repeated horizontal work for every destination row.
- WinRT completion used repeated status calls and two-millisecond sleeps.
- Plugin transport used the screenshot encoder's balanced PNG compression.
- Verbose geometry logging issued many small synchronous writes.
- Existing portable configurations contain duplicate shortcut assignments.

## Changes

- Compare exact masked, scaled pixels before reusing OCR results.
- Retain the region, scale, and mask in the reuse key. Clear reuse state after settings changes and freeze transitions.
- Bound pixel storage to 16 MiB per generation, with two generations. Larger reads still run OCR.
- Replace matching cache entries during promotion. A failed recognition cannot make older pixels satisfy a newer read.
- Reuse one DXGI staging texture for the current crop size.
- Back off failed DXGI attempts for one second on the same monitor. BitBlt continues capturing fresh pixels.
- Expand one row, then copy its vertical repetitions. Preserve every color byte and the existing opaque alpha policy.
- Wait for WinRT completion notification. Keep the five-second deadline and request cancellation after timeout.
- Use fast, lossless PNG compression for plugin requests. Keep screenshot-file compression unchanged.
- Batch verbose OCR records into one write. Log Windows arm-state transitions only when the state changes.
- Record popup measurement, show, and paint time. Record MeikiOCR decode, inference, and geometry time.

No model, capture extent, default scale, dependency, trait, golden, or quality floor changed.
Thread sweeps did not justify a reliable default change. Both installations retain four MeikiOCR threads and their original plugin configuration.

## Measured results

Each fixed-image comparison uses 12 warm samples after one cold sample.
The paired image contains the local Japanese corpus's solid and outlined text, at 1080 by 240 physical pixels.
The host also ran Linux regression work. These samples demonstrate the tested workloads, not a universal latency guarantee.

| Engine | Scale | Baseline median | Final median |
| --- | ---: | ---: | ---: |
| Windows OCR | 1 | 14.98 ms | 16.45 ms |
| Windows OCR | 2, production default | 27.00 ms | 24.54 ms |
| MeikiOCR | 1 | 131.83 ms | 112.79 ms |
| MeikiOCR | 2, production default | 153.15 ms | 129.55 ms |

All repeated outputs retained identical text and geometry. Geometry hashes also matched between baseline and final builds, for both engines and scales.
The Windows 1x sample was slower. Do not claim an improvement for every image or scale.

Repeated real-screen capture plus OCR fell from **37.27 ms to 11.72 ms median**, approximately 3.2 times faster.
The before and after BMP hashes match: `c8f3e6d5254ed2d098f79640c0f613504b4099dbf4182f5cde23d25575467f3c`.
Each final warm read reused OCR results. Capture still ran, so changed screen pixels remain observable.

The real Worker, dictionary, and Windows renderer also resolved the visible fixture and painted the popup:

- A warm Windows sample reached paint completion in 46.64 ms.
- A warm MeikiOCR sample reached paint completion in 194.18 ms.
- A second MeikiOCR fixture position reached paint completion in 151.04 ms.
- Popup measurement, show, and paint took approximately 4–5 ms in these samples.
- MeikiOCR startup still took about two seconds. Fresh-image inference remains the main MeikiOCR cost.

Paint completion does not measure physical display scan-out.

## Shortcut conflicts

Settings reject conflicting assignments before saving or applying dictionary changes.
Windows compares virtual-key aliases and modifier requirements. Linux compares normalized configured chords.
Disabled actions do not reserve keys. A bare Windows lookup or Anki key conflicts with a chord using the same terminal key.

Existing Windows conflicts disable the lower-priority binding in memory and emit a diagnostic. The saved configuration remains unchanged.
Priority is Back/Escape, lookup, Anki add, static region, screenshot, then OCR clipboard.
Clearing a binding now clears its previously installed Windows hook binding.
Linux compositor bindings remain external configuration; the app validates its own configured chords.

Native keyboard testing opened an isolated settings file and activated Apply with conflicting F2 assignments.
Apply reported `Static region conflicts with Lookup trigger. Choose different keys.`
The file hash remained `e509f0fd81ac757bb254b73cc3733f203eef817058884fb3a64c1f2cc00865df`.

The primary nightly suppresses its duplicate screenshot binding at runtime.
The Japanese nightly suppresses its duplicate screenshot and OCR-clipboard bindings at runtime.
Choose distinct keys in Settings to enable those bindings again.

## Verification

- Three complete Windows sweeps: 2,107 passed, five failed, and 16 ignored in each sweep.
- The unchanged baseline reproduces all five failures, with 13 geometry tests passing.
- The failing geometry names are `bordered_pill`, `full_chrome`, `nested_list`, `pitch_single`, and `wrapping_heavy`.
- No golden was changed or relaxed.
- The golden dictionary test uses `CHIBIPOP_GOLDEN_DB` pointing at the compatible nightly database.
- The repository-local database has schema 2. It was preserved; this build requires schema 4.
- Both documented Windows clippy passes meet their existing counts: one accepted warning, then zero diagnostics.
- Python tooling: 53 tests passed.
- Linux runner: 26 steps passed, zero failed, and four were unavailable.
- Linux passed three workspace sweeps, all ten OCR quality tests, both clippy passes, release build, and packaging.
- Linux also exercised wlr capture, surfaces, clipboard, and degradation paths.
- The final cache-promotion repair passed 56 focused Linux source tests in a separate copy of the current source.
- Native settings conflict rejection passed through keyboard computer use.
- Real Worker-to-popup tests passed for Windows OCR and MeikiOCR. The MeikiOCR popup image was visually inspected.

The Linux full run used the snapshot preceding the final platform-neutral cache-promotion repair.
The separate 56-test run verified that final repair without changing the active full-run snapshot.

## Remaining acceptance and limits

- Five pre-existing Windows geometry goldens still fail on this machine. A clean overall Windows gate is not claimed.
- Native screenshots fail with `SetIsBorderRequired ... 0x80004002`. Native mouse input reports unavailable window geometry.
- Keyboard computer use works. Pointer-driven hold, toggle, and per-character hover acceptance remains unrun.
- The headless Linux environment lacks the real cursor and ScreenCast portal capabilities required by four runner steps.
- Models and inference behavior remain unchanged. Fresh MeikiOCR requests can still exceed 100 ms.
- Wider regions and dense text require more work. No capture region was reduced to manufacture a speed improvement.

## Nightly delivery

Both portable nightlies were refreshed and started with their original configurations.
Both captured real screen pixels through DXGI and completed OCR.
Only `chibipop.exe` and `plugins/meikiocr/adapter.py` needed copying.
All 53 primary and 56 Japanese protected files matched before and after refresh.
Configurations, dictionaries, libraries, and plugin settings were preserved. Previous program files remain backed up in the evidence directory.

Both executable hashes are `d0b0300653b5f24b7d957c3cda477b53cc414f77284a0dd6a975d6cbbbc28460`.
Diagnostic processes were stopped after testing.

## Reproduction and evidence

Raw logs, images, manifests, measurements, and replay scripts are under `regression-artifacts/ocr-performance-2026-09-06/`.
`measurements.json` records samples and geometry hashes. `refresh-report.json` records program changes and preservation checks.
The Linux report is `linux-final/linux-regression-report.json`.

```powershell
$env:CHIBIPOP_GOLDEN_DB = '<compatible-install>/data/chibipop.sqlite'
cargo test --workspace --exclude chibipop-linux --no-fail-fast

$env:CHIBIPOP_BENCH_PLUGIN = '<install>/plugins/meikiocr'
cargo test --release -p chibipop-windows --test ocr_performance -- --ignored --nocapture

$env:CHIBIPOP_BENCH_INSTALL = '<compatible-install>'
$env:CHIBIPOP_BENCH_POINT = '<physical-x>,<physical-y>'
cargo test --release -p chibipop-windows --lib live_fixture_reaches_popup_paint -- --ignored --nocapture

python scripts/linux_container_regression.py --runtime podman --loops 1 --artifacts-dir <output-directory>
```

Unset `CHIBIPOP_BENCH_PLUGIN` to select Windows OCR in the live popup test.
The fixed-image test always includes Windows OCR and adds MeikiOCR when the plugin path is set.
Set `CHIBIPOP_BENCH_BMP` to replay a 24-bit or 32-bit BMP. Otherwise the test uses the committed small Japanese fixture.

## Pending-key editor follow-up

The Windows General tab now exposes the screenshot shortcut alongside the capture controls.
Apply compares the complete pending configuration. Users can repair saved conflicts or swap keys in one Apply.
Rejected edits retain every pending selection. Startup conflict protection remains enabled, as requested.
The initial text editor accepted modifier chords, single keys, or an empty value. The button follow-up below replaces that editor.
Invalid shortcut text is rejected before saving. Unedited platform fields remain unchanged.

The updated Windows sweep recorded 2,111 passed tests and the same five baseline geometry failures.
Both clippy passes met their existing counts. The release build and settings audit passed.
The real settings-window regression covers pending key swaps, saved conflict repair, and duplicate rejection.
Linux's full container run was not repeated for this incremental editor update; shared form-preservation tests passed on Windows.

Both nightlies were refreshed again with protected manifests unchanged: 54 primary files and 57 Japanese files.
Their new executable hash is `153d408c26977ac756d2f561288040ed06aa92d8c35a451ec5d25ad1dc97e276`.
The follow-up evidence uses `pending-keys-*` logs and the `pending-keys-delivery` directory in the same artifact root.

## Entry screenshot key button follow-up

General now labels the control **Entry screenshot key**. Click it, then press a single non-modifier key.
Escape cancels capture. Clear disables the binding. Existing saved modifier chords remain unchanged until the user rebinds or clears them.
Apply still validates all pending assignments together. Startup protection remains enabled.

The description explains that an open dictionary entry is required. The action saves a PNG using the configured screenshot mode.
It also adds the entry to Anki when connected. The screenshot field mapping controls whether the note receives the image.

| Action | Purpose |
| --- | --- |
| Entry screenshot | Save an image for the open entry, and add the entry to Anki when connected. |
| Lookup trigger | Read screen text and show its dictionary entry. |
| OCR clipboard | Select a region and copy recognized text to the clipboard. |
| Static-region key | Set the fixed OCR area used by Static sentence-capture mode. |
| Add to Anki | Add the current entry; screenshot-on-add follows its separate configuration. |

The Windows sweep recorded 2,112 passed tests and the same five baseline geometry failures.
Both clippy gates, the release build, and the settings audit passed.
Real-window tests verified capture, cancellation, clearing, saved-chord preservation, pending swaps, and duplicate rejection.
The audit confirmed a visible button, Clear button, and description.

Both nightlies were refreshed with protected manifests unchanged: 54 primary files and 57 Japanese files.
Their executable hash is `73bc52d1c811f1dd32ef3bf69bdf6d7b1c5a3cc21dfba19d449f25c14a3a1fd2`.
Evidence uses `screenshot-button-*` logs and `screenshot-button-delivery` in the same artifact root.
