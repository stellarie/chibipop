# Accuracy and polish — acceptance

**Task:** accuracy-and-polish plan, Task 6 (spec §2). **Measures; does not tune.**
**Date:** 2026-07-28
**Build:** release at `274ec70` + the probe-instrument commit that follows it.
**Screen:** portrait secondary only (x ≥ 2560).

## Scope of this round, and why it is smaller than the plan

Tiling was deferred to `docs/BACKLOG.md` by the user's explicit decision: single-pass is accurate
today and the tiling rework is not worth gating three working changes on. So:

| Spec acceptance item | Status |
|---|---|
| §2.1 tiling accuracy (9-hover sweep at `--tiles 3`) | **Deferred** — the change it tests was not made |
| §2.2 match highlight | **PASS**, with a named caveat on the driving mechanism |
| §2.3 vertical text | **Measured** — `2026-07-28-vertical-text-measurement.md` |
| §2.4 identity (tray icon) | **PASS** — Task 1, `a16081a..e4ec092` |

## §2.2 — the match highlight

### Prediction, recorded before running

Hovering (2743, 270) — the character 宿 in `駅長は宿舎の方へ手の明りを振り向けた。` in a ttsu-reader
window — the word boxes are 宿 `x=2730 y=257 w=26 h=27` and 舎 `x=2758 y=257 w=26 h=27`, the top
hit is 宿舎 with `match_len=2`, and `HIGHLIGHT_PAD` is 3. The box must therefore be
**x=2727 y=254 w=60 h=33**.

### Observed

```
at:      byte 9 -> Some('宿')
anchor:  x=2730 y=257 w=26 h=27
1. 宿舎 [しゅくしゃ]  freq=21663  match=2  score=4.77
match:   x=2727 y=254 w=60 h=33  (2 chars)
```

Exactly the prediction. Drawn on screen and captured at full resolution
(`scratchpad/hl_live.png`, zoomed in `hl_zoom.png`): the bright blue Match frame surrounds
**宿舎 and nothing else**, padded clear of the ink; the olive Anchor frame inside it covers only
the hovered 宿, which is the correct distinction between "the character you are on" and "the word
being defined".

### The deconjugation case, which is the one that justifies the feature

Hovering 風邪 at (3080, 231):

```
1. 風邪をひく [かぜをひく]  freq=11655  match=6  score=7.22
match:   x=3063 y=207 w=177 h=33  (6 chars)
```

The box spans all six characters of **風邪をひいて** for the dictionary entry 風邪をひく. The
highlight follows the *match*, not the headword, so an inflected idiom is shown as the single
thing it is. This is not reconstructable from the popup text alone, which is the argument for
drawing it.

### Exactly one box on the default path

Verified by construction and by test, not by eye:

- `resolve_at_tiled_scanned(cursor, collect)` is called with `collect = scan_display.captures`,
  which is `[debug] show_scan_region` alone. With it off, **no capture rectangle is ever
  constructed** — "off" is inert, not collected-and-filtered.
- `ScanDisplay` makes that a checkable statement rather than a convention; four tests in
  `geom.rs` cover the four combinations of the two settings.
- The only rectangle pushed on the default path is `ScanKind::Match`.

`probe --show-region` deliberately draws **both** — it is the debug instrument, and the screenshot
above shows three frames for that reason.

### The upgrade path, proven against the real file

`target/release/chibipop.toml` on this machine has **no `highlight_match` line** — it predates the
field. Launching `run` against it creates `ChibipopOverlayClass`, which only happens when
`scan_display.any()` is true, i.e. the field defaulted to `true`. A bare `#[serde(default)]` would
have yielded `false` and the feature would have worked only on fresh installs. The live file was
not edited.

## ⚠️ What was NOT verified, and why

**The app's own hover path was never exercised.** `chibipop run` installs a `WH_MOUSE_LL` hook,
and **synthetic input from this environment does not reach it** — neither `SetCursorPos` (which
does move the pointer; verified by reading it back) nor `SendInput` with
`MOUSEEVENTF_ABSOLUTE | VIRTUALDESK`, from an in-process call or from a detached
`Start-Process`. Across every attempt the process stayed at 2.6 MB private with all three windows
`visible=False`, i.e. zero hovers were processed. The tool sandbox's job object is the suspect.

Consequently the following is verified only through `probe`, which calls **the same
`present::match_highlight`** — deliberately shared so the instrument cannot disagree with the app:

- `match_highlight`, `ScanKind::Match`, the `scan_match` colour, the overlay's drawing of it.

And the following is verified only by unit test and by reading:

- the four-line call site in `app.rs::resolve_trigger` that pushes the rect;
- **spec D7's real cost** — with the highlight on, the overlay window now exists on every hover,
  adding a second hide/restore per capture under the capture guard. Its cost *at rest* was
  measured and is nil (see below); its cost *per hover* was not.

`chibipop watch`, which polls the cursor instead of hooking it, **can** be driven and was — see
the resource survey. It exercises capture → OCR → resolve → lookup, but not the popup renderer.

**For oniichan:** the one-command check is

```bash
./target/release/chibipop.exe probe --at 2743,270 --tiles 1 --show-region 8
```

then hover the same spot by hand with `run` up, and confirm you see **one** box, not three.
