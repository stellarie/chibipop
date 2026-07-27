# Two-pass OCR — live acceptance measurement

Branch `feat/m3-popup`. Binary: `target/release/chibipop.exe`.

**Binary freshness — rebuilt, not assumed current.** The shipped binary predated HEAD:
`chibipop.exe` was built at 01:31, but HEAD (`2d53e10`, "style(text): satisfy clippy in
split_at_clipped without changing behaviour") touched `src/text/layout.rs` at 01:43. The
commit message claims no behaviour change, but that is exactly the kind of claim this task
exists to verify rather than trust, so it was rebuilt (`cargo build --release`, 6.05s,
incremental) before any measurement. Confirmed current afterward: no file under `src/` is
newer than the binary.

**Verdict up front, not buried:**

| # | Criterion | Measured | Verdict |
|---|---|---|---|
| 1 | Accuracy: `--tiles 3` ≥ `--tiles 1`, and `--tiles 1` stays 10/10 | 10/10 and 10/10 | **PASS** |
| 2 | Reading distance: ≥20 forward characters | best achievable on this line: **17** | **FAIL** — not text-capped, see below |
| 3 | Latency: within +150ms | **+37ms** median delta (108→145ms) | **PASS** |
| 4 | Kill switch: `max_ocr_passes = 1` ≡ `--tiles 1` | equivalent by code path + passing config round-trip test | **PASS** (methodology note below — no live hover test was performed) |

Criterion 2 misses plainly. It is not softened below.

## Text used

Portrait secondary monitor (x ≥ 2560), the line the task pointed at: y ≈ 1359–1385,
running from x = 2873 rightward, horizontal orientation, characters ~26px tall, average
horizontal pitch ~28px (measured from the OCR's own word boxes, not assumed). This is
**not** the ~40px visual-novel text the design spec's "≥20 characters" and "~7 characters"
baselines were calibrated against (`docs/superpowers/specs/2026-07-27-two-pass-ocr-design.md`
§1/§2) — smaller text, flagged up front per the task's own warning.

Ground truth (given): `に寂しくて参るだろうよ。若いのに可哀想だな」` — 22 characters, `に` through `」`.
OCR additionally reads `こんなところ、今` immediately before `に` on the same line; that prefix
was not independently given as truth and is not scored, it's mentioned only because it
explains where the anchor sits inside the line.

## 1. Accuracy — 10/10 vs 10/10

Ten centres computed as the midpoint between consecutive leading edges of the given word
boxes (so each centre is unambiguously inside its own glyph):

```
01_ni    x=2885 y=1372  truth=[に]  tiles1=[に] OK  tiles3=[に] OK
02_saku  x=2914 y=1372  truth=[寂]  tiles1=[寂] OK  tiles3=[寂] OK
03_shi   x=2945 y=1372  truth=[し]  tiles1=[し] OK  tiles3=[し] OK
04_ku    x=2971 y=1372  truth=[く]  tiles1=[く] OK  tiles3=[く] OK
05_te    x=2996 y=1372  truth=[て]  tiles1=[て] OK  tiles3=[て] OK
06_san   x=3026 y=1372  truth=[参]  tiles1=[参] OK  tiles3=[参] OK
07_ru    x=3054 y=1372  truth=[る]  tiles1=[る] OK  tiles3=[る] OK
08_da    x=3082 y=1372  truth=[だ]  tiles1=[だ] OK  tiles3=[だ] OK
09_ro    x=3112 y=1372  truth=[ろ]  tiles1=[ろ] OK  tiles3=[ろ] OK
10_u     x=3140 y=1372  truth=[う]  tiles1=[う] OK  tiles3=[う] OK
---
tiles1 score: 10/10
tiles3 score: 10/10
```

Extraction used the brief's own multibyte-safe pattern (`sed -n "s/^at: .*-> Some('\(.*\)').*/\1/p"`),
which captures the whole quoted grapheme rather than one byte — the known trap in this
environment. **Criterion 1: PASS**, cleanly — no regression, and tiles=3 matches tiles=1
character-for-character on this sweep.

## 2. Reading distance — FAIL, not text-capped

Two anchors were measured on the same line, because the result is anchor-dependent and one
anchor alone would have been misleading.

### Anchor A — leftmost available position, `に` (x=2873, hover 2885,1372)

22 characters are available forward from here to the line's actual end (the full ground
truth, `に` through `」`) — the only anchor on this line with enough room to potentially
clear the 20-character bar.

Raw output, `--tiles 1`:
```
cursor:  (2885, 1372)
region:  x=2635 y=1322 w=500 h=100
ocr line 0: "こんなところ、今に寂しくて参るだろこ"
line:    こんなところ、今に寂しくて参るだろこ
at:      byte 24 -> Some('に')
```
Forward from the resolved `に`: `に寂しくて参るだろこ` — **10 characters**, last one wrong
(`こ` for truth's `う`, a boundary-clipping error of exactly the kind two-pass exists to fix).

Raw output, `--tiles 3` (reproduced 3 times, byte-identical every run):
```
tiled:   "に寂しくて参るだろうよ。若いのに可"  (17 chars)
```
All 17 characters are correct against truth. But this is **5 characters short of the
line's actual end** (`哀想だな」` never appears) — and those 5 characters are demonstrably
real, OCR-legible text, not blank space (see Anchor B below, which reads them correctly).
So this shortfall is **not the text-running-out trap** the task warned about; the text
was there and readable, just not reached from this anchor.

Isolating which tile is responsible: `--tiles 2` (one tile only) from the same point
produces the **exact same 17 characters**:
```
tiled:   "に寂しくて参るだろうよ。若いのに可"  (17 chars)
```
So the second tile contributed **zero** characters here. Reading `src/text/layout.rs`'s
`tile_forward` (not modified, read only): tile 1 spans `[2873, 3373)`, keeps everything
whose trailing edge clears `EDGE_MARGIN` of that boundary, and hands the clipped word's own
leading edge to tile 2 as its start (`split_at_clipped`, unit-tested, unchanged). By the
measured ~28px pitch, `可` (position 17) fits inside tile 1 and `哀` (position 18) straddles
its edge — consistent with what was kept. Tile 2 would then span roughly `[3348, 3848)`,
which by the same pitch estimate should contain `哀想だな」` with room to spare. It returned
nothing. The most likely explanation, and one already documented in this codebase rather
than invented here, is `layout.rs`'s own `REGION_W` comment: *"the same screen text
recognises differently when the box shifts by 50 pixels"* — tile 2's crop is mostly blank
margin with a small cluster of glyphs near one edge, a framing Windows' OCR is already
known (by this project's own prior measurements) to handle inconsistently. This is offered
as the likely mechanism, not a certainty — `probe` does not expose a per-tile diagnostic,
so the tile-2 capture itself could not be inspected directly without adding instrumentation,
which is out of scope for a measurement task.

### Anchor B — mid-line, `ろ` (x=3100, y=1372) — the task's own sanity-check point, re-run independently

Only 14 characters are available forward from here to the line's actual end.

Raw output, `--tiles 1`:
```
cursor:  (3100, 1372)
region:  x=2850 y=1322 w=500 h=100
ocr line 0: "に寂しくて参るだろうよ。若いのに可ー"
line:    に寂しくて参るだろうよ。若いのに可ー
at:      byte 24 -> Some('ろ')
```
Forward: `ろうよ。若いのに可ー` — **10 characters**, with the tail collapsed into a single
wrong glyph (`ー`) standing in for what should have been at least 4 more distinct characters.

Raw output, `--tiles 3`:
```
line:    に寂しくて参るだろうよ。若いのに可ー
at:      byte 24 -> Some('ろ')
tiled:   "ろうよ。若いのに可哀想だな」"  (14 chars)
```
All 14 correct, reaching the true end of the line exactly. This independently reproduces
the task's own stated sanity check byte-for-byte. **14 is the maximum physically available
from this anchor** — this is the legitimate text-exhaustion case the task warned about, and
here the feature used all of it correctly.

### Verdict

The only anchor with enough physical text to potentially demonstrate ≥20 (Anchor A, 22
available) achieved **17**, short of the criterion, for a reason independent of text
availability. The other anchor is correctly capped by the line's own length below 20
regardless of the feature. **Criterion 2: FAIL on this line** — no anchor here can show a
clean pass, and the one that comes closest misses by 3, not for the reason the task's trap
warning describes. Per the task's instruction not to go hunting for other text, no other
line was searched. Both single-pass baselines measured here (10 chars at both anchors) ran
higher than the design spec's original "~7 characters" VN-text figure — expected, since
this text is smaller — but two-pass's real improvement (+7 at Anchor A, and a fully correct
14/14 at Anchor B) still falls short of the ≥20 bar on this specific line.

## 3. Latency

Same real hover point (3100,1372), 5 runs per setting:

```
tiles=1 run1: 108 ms
tiles=1 run2: 106 ms
tiles=1 run3: 105 ms
tiles=1 run4: 112 ms
tiles=1 run5: 113 ms
tiles=3 run1: 145 ms
tiles=3 run2: 146 ms
tiles=3 run3: 144 ms
tiles=3 run4: 145 ms
tiles=3 run5: 144 ms
```

Medians: tiles=1 = **108ms**, tiles=3 = **145ms**. Delta = **+37ms**.

Both absolute numbers are well under the design spec's original ~230ms baseline (233/231/235ms,
per the spec's §1.1) — likely warmer OS/file caches on this run, not a discrepancy that
affects the criterion, since the criterion is the delta between two figures measured
together, under the same conditions, in this same session. **Criterion 3: PASS**, with
large headroom (+37ms against a +150ms budget).

## 4. Kill switch

**Methodology note, disclosed up front:** the task brief's Step 5 says to restart chibipop
and confirm "from the same hover" — but doing that literally means launching
`chibipop.exe run` and moving the real cursor onto the text, which this task's own hard
screen-constraint rule forbids (no cursor movement, no opening/focusing windows on the
user's primary monitor — and `run` in `live` trigger mode would actively OCR-capture
whatever the user is doing as the real cursor moves). `probe --at` exists precisely to avoid
needing the real cursor, but `probe` does not read `chibipop.toml` at all — `Command::Probe`
in `src/main.rs` takes `--tiles` as its own CLI argument (default 3), entirely independent
of the config file. Only `Command::Run` reads `cfg.ocr.max_ocr_passes`. So the two can't be
bridged by literally running both and comparing — instead, this was verified by code-path
identity, which is read-only and covers every hover point at once rather than the one point
a manual test could sample:

- `src/app.rs:249` reads `cfg.ocr.max_ocr_passes` and passes it into `worker_main`, which at
  `src/app.rs:501` calls `OcrTextSource::new(max_ocr_passes)`, and at `src/app.rs:623/627`
  calls `ocr.resolve_at_tiled(cursor)`.
- `src/main.rs:97/158` (the `probe` tiled branch) calls `OcrTextSource::new(tiles)` and
  `source.resolve_at_tiled(cursor)` — the identical type, identical constructor, identical
  method, differing only in which caller supplies the `u8`.
- `src/text/ocr.rs:188-190`: `resolve_at_tiled` early-returns pass-1's own span whenever
  `self.max_passes <= 1`, before any tiling logic runs. This is the one branch that decides
  the kill switch, and it depends only on the stored `u8`, not on how it got there.

Given that, `cfg.ocr.max_ocr_passes = 1` and `--tiles 1` are provably the same code path,
not just expected to behave alike.

Config parsing was verified rather than assumed: `src/config.rs`'s existing
`a_single_pass_round_trips` test (unchanged, pre-existing) saves `max_ocr_passes = 1` and
reloads it — re-ran the whole `config::` module now, on this HEAD:

```
running 9 tests
test config::tests::capture_exclusion_defaults_to_false ... ok
test config::tests::defaults_match_the_spec ... ok
test config::tests::ocr_passes_defaults_to_three ... ok
test config::tests::a_missing_file_is_created_with_defaults ... ok
test config::tests::malformed_toml_errors_naming_the_file ... ok
test config::tests::a_config_written_before_the_ocr_section_existed_still_loads ... ok
test config::tests::an_empty_ocr_section_still_defaults_max_ocr_passes_to_three ... ok
test config::tests::a_saved_non_default_mode_survives_a_round_trip ... ok
test config::tests::a_single_pass_round_trips ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 117 filtered out
```

The live `target/release/chibipop.toml` was then actually edited (not just reasoned about):
backed up (md5 `83ed69cb58a5cc3b96dfb4786af03705`), a `[ocr]\nmax_ocr_passes = 1` section
appended, read back to confirm it wrote as intended, then restored. Restoration verified
byte-for-byte — the checksum of the restored file matches the pre-edit backup exactly
(`83ed69cb58a5cc3b96dfb4786af03705` both before and after).

**Criterion 4: PASS**, on the strength of code-path identity plus a passing config
round-trip test — not on an end-to-end manual hover, which the screen constraint ruled out.

## Files touched

- This note (new).
- `target/release/chibipop.toml` — edited then restored byte-for-byte during Step 4;
  confirmed identical (md5 match) at the end. Not a tracked file; no diff exists to show.
- No `src/` file was modified. `Cargo.toml` carries a pre-existing uncommitted formatting-only
  diff (multi-line feature list, same features) unrelated to this task, left untouched.
