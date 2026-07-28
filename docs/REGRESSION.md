# chibipop — regression checklist

Run this after any large change. It is ordered cheapest-first: **if a tier fails, stop and fix
before running the next one.**

Everything here was verified working on 2026-07-28. Numbers are what was actually measured on this
machine, not targets — a *different* number is not automatically a failure, but it is always worth
explaining before dismissing.

---

## Tier 0 — the automated gate (~2 min, no screen)

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup
powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | grep -E "^test result"
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -cE "^error: (doc list|explicit call|this function|this loop)"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

| Check | Expected |
|---|---|
| Rust tests | **all green**, ~239 total across 5 targets |
| Clippy | **exactly 5** accepted errors |
| Bin-target clippy (below) | **0** |
| Release build | Finished, no errors |

The bin target needs the five accepted lints suppressed or clippy aborts before `main.rs` compiles:

```bash
cargo clippy --all-targets --all-features -- -D warnings \
  -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion \
  -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -cE "^(error|warning)"
```

Python dictionary builder, if `tools/build-dict` changed:

```bash
cd tools/build-dict && python -m unittest discover -s tests    # 48 tests
```

**Run the suite 3× if anything touched a `static`.** Cargo runs tests in parallel threads of one
process, and a shared static produces an intermittent red that a single run will miss — this
happened once, with the wheel accumulator.

---

## Tier 1 — agent-verifiable, on real pixels (~5 min)

Needs Japanese text on the **portrait secondary** (x ≥ 2560). `probe` reads a coordinate without
moving the pointer, so it disturbs nothing.

### 1.1 The pipeline resolves and looks up

```bash
./target/release/chibipop.exe probe --at <x>,<y> --tiles 1
```

Expect `orient:`, a `line:`, an `at: byte N -> Some('X')`, an `anchor:`, ranked hits, and a
`match:` box. Horizontal prose at ~26px should be **exact**.

### 1.2 The match highlight is where it claims

Hover a two-character word and check the box is the union of both glyph boxes, padded 3px. Worked
example, with real numbers:

- 宿 at `x=2730 y=257 w=26 h=27`, 舎 at `x=2758 y=257 w=26 h=27`, top hit 宿舎 `match=2`
- → `match: x=2727 y=254 w=60 h=33`

**Predict the rect from the word boxes before running it.** A highlight that merely looks plausible
is the failure this catches.

### 1.3 The deconjugation case

Hovering 風邪 must box **all six** characters of 風邪をひいて for the entry 風邪をひく — the
highlight follows the *match*, not the headword.

### 1.4 Draw it and look

```bash
./target/release/chibipop.exe probe --at <x>,<y> --tiles 1 --show-region 8
```

Screenshot the region while it is up. `probe` draws the capture boxes **and** the match box; the
app on shipped defaults draws only the match box. Both being drawn here is correct, not a bug.

### 1.5 Same-glyph stability (the anti-flicker precondition)

Probe 4–5 points **inside one glyph** and diff the full hit lists. They must be **identical**, and
the anchor must not move. The `line:` tail *will* wobble as the capture region slides — that is
expected and is exactly why the dedupe keys on content, not on the line.

### 1.6 Vertical text is still broken in the known way

```bash
for r in "500,100" "100,500"; do ./target/release/chibipop.exe probe --at <x>,<y> --region "$r"; done
```

At `500,100` on a vertical column, expect either no resolution or a **fabricated** cross-column
sentence (e.g. `いユし日`). At `100,500`, expect the correct column. **If the default shape ever
starts working, something changed — investigate rather than celebrate.**

### 1.7 The wheel is not swallowed at rest

With `run` live and no popup up, park the pointer over a scrollable window and scroll. **The page
must scroll.** This is the worst-case failure in the whole app: a stuck arm kills the wheel for
every application.

*(An agent can drive this: `mcp__Windows-MCP__Scroll` injects real wheel input. `SendInput` from a
tool shell returns 0 and cannot.)*

### 1.8 Resources

```bash
ls -l target/release/chibipop.exe          # ~3.3 MB, limit 100 MB
```

| Measurement | Value |
|---|---|
| `run`, idle | 12 MB WS / 2.6 MB private, **0.000%** CPU |
| `watch`, 417 hovers, 3-pass | plateaus 37 MB WS / 15 MB private, handles flat at 209 |
| Startup to live tray icon | **0.18 s** |

⚠️ `run` under sustained **real** hovering has never been re-measured and recorded 94.8 MB at M3.
That is the one number still outstanding.

---

## Tier 2 — human-only (~5 min)

**None of this can be automated.** Synthetic mouse *movement* cannot reach a global `WH_MOUSE_LL`
hook — `SendInput` returns **0**, the call is rejected. Print that return value first if anyone ever
doubts it.

1. **Hover** Japanese text → popup appears beside it.
2. **Reach into it** — move the cursor from the word into the popup. It must not change or vanish.
3. **Leave it** → normal hovering resumes, no dead patch of screen.
4. **Jiggle on a word** → no flicker.
5. **Scan sideways** to the next word → the next word resolves. *(Fails if the sticky region is
   ever widened to a bounding box.)*
6. **Scan along a conjugated verb** (振り向けた) → **one** popup across all of it, not one per
   character.
7. **Overflowing entry** → thin scrollbar at the right edge; wheel scrolls it end to end and the
   thumb ends flush.
8. **Hover a word, do not move, wheel** → **the page underneath scrolls.** *(This is the D7 defect:
   arming on the whole sticky region would freeze the page whenever you hovered while reading.)*
9. **Tray menu open + wheel** → wheel still works. *(D9: `TrackPopupMenuEx` pumps its own loop and
   discards `WM_TIMER`, so the arm cannot be recomputed while it is open.)*
10. **Quit chibipop, then wheel** → still works. The one failure that would outlive the app.
11. **`chibipop run` → the settings window opens by itself**, **both buttons visible**, values match
    the TOML, controls look native (not Windows-95 grey — that is the manifest). Cancel dismisses it
    and hovering works normally underneath.
11a. **Right-click the tray icon → still shows NOTHING.** Known broken, BACKLOG 7. *If a menu ever
    appears here, something changed — find out what before celebrating.* Note the icon lives behind
    the `^` chevron, not in the visible tray.
11b. **`chibipop settings`** → the same window, captioned **"Apply"** and "Restart chibipop to use
    them" rather than "Apply & Restart". A caption mismatch means the `restarts` flag is wrong.
12. **Reorder dictionaries → Apply** → order changes, and **`chibipop.toml` still holds the
    original substrings**, merely reordered. Invisible from the UI; check the file.
13. **Open Settings, touch nothing, Apply** → the TOML is unchanged apart from formatting.

---

## Traps that keep recurring

Each of these has bitten at least once. They are cheap to check and expensive to rediscover.

| Trap | Tell |
|---|---|
| **`CreateWindowExW` takes the OUTER size** | Content laid out past the client area. Caption+frame is 16×39 at 96 DPI. Size from measured content via `AdjustWindowRectEx`. |
| **First `ShowWindow` obeys `STARTUPINFO`, not you** | A window created and sized but `WS_VISIBLE` never set. Launch-hidden makes Settings do nothing. Show via `SetWindowPos(SWP_SHOWWINDOW)`. |
| **`&` in a button caption is an accelerator** | "Apply & Restart" renders "Apply ‗Restart". Double it. |
| **Nested message pumps eat `WM_TIMER`** | `TrackPopupMenuEx`, `MessageBoxW`, `DialogBoxParamW`, caption drag. The wheel arm latches. Disarm before any of them. |
| **`display_order` holds substrings, not names** | Order works today, silently stops after the next dictionary rebuild. Never write live names back. |
| **A task that adds a field must be the task that reads it** | `field never read` = a 6th clippy error against a 5-error gate. |
| **Ghost tray icons** | A force-killed instance leaves a corpse; right-clicking it does nothing. Sweep the cursor over the tray to reap them. |
| **`cargo fmt` is not run here** | The repo has never been rustfmt-clean. Do not "fix" it. |
| **`Cargo.toml` carries an unstaged reformat** | Never `git add -u`/`-A`. Stage by name. |

## When something fails

**Instrument, do not theorise.** The `ShowWindow` bug survived three confident hypotheses and fell
in one run of `eprintln!` printing the actual return value and window style. The general shape:
post the message by hand to isolate delivery from handling, log the Win32 return values, and only
then reason.
