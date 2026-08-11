# chibipop — regression checklist

Run this after any large change. It is ordered cheapest-first: **if a tier fails, stop and fix
before running the next one.**

Everything here was verified working on 2026-07-28, and tier 2 was re-confirmed on 2026-07-29. Numbers are what was actually measured on this
machine, not targets — a *different* number is not automatically a failure, but it is always worth
explaining before dismissing.

**Three exceptions to "verified", all marked in place.** Tier 1 items **1.9–1.13** were added
2026-08-09 with the resizable-capture / hot-reload branch and **have not been run**. Items
**1.14–1.15** were added 2026-08-11 with the per-character-retrigger / OCR-language branch and
were **partially run the same day** — 1.14's core and all of 1.15 passed on one machine in
horizontal text; the tategaki case, hold-key inertness, drill-down/scroll preservation and the
missing-recognizer paths were **not** exercised. See the callout above 1.14 for exactly what was
and was not covered. Item **11b** was corrected 2026-08-09, having described behaviour
that never existed in any version of the program.

---

## Tier 0 — the automated gate (~2 min, no screen)

**This tier is the CI contract.** The commands below are unchanged since 2026-07-29 and are what
`.github/workflows/ci.yml` runs; the numbers beside them were last re-measured on **2026-08-11**.
Two of them are `grep -c` counts rather than exit codes, and that is deliberate — see the note
under the table. CI additionally passes `--color never` and runs the suite three times; both of
those are explained in the callouts below, and neither is optional there.

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup
powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | awk '/^test result: ok\./ {s+=$4} END {print "TOTAL:", s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error" | grep -vc "could not compile"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

| Check | Expected |
|---|---|
| Rust tests | **all green**, **710** total across **6** targets, 1 ignored (was 698; re-measured 2026-08-11) |
| Clippy | **exactly 3** accepted errors (was 4; see below) |
| Bin-target clippy (below) | **0** |
| Release build | Finished, no errors |
| Apply handler | under **50 ms** (`LowLevelHooksTimeout` is 300 ms) |

**The test count is a floor, not an equality.** Adding a test must not break CI; a whole target
silently not running must. CI asserts `≥ 400` and prints the total; **710** is what this machine
measures today, so a *lower* number is the thing to explain. The clippy counts are equalities —
that is the difference between the two rows and it is deliberate.

**670 → 698 is a re-baseline, not a finding**, and it is recorded here in the commit that moves it,
per the rule in the callout below. The per-character-retrigger / OCR-language branch added 28 tests
across seven tasks and removed none; the count rose monotonically with each one (673, 675, 677,
685, 687, 688, 698) and the rename in the last task left it unchanged, as a rename must. The three
runs above the table reported **698, 698, 698** — identical, which is the point of running it three
times. The clippy counts did **not** move: still 3 raw and 0 on the bin target.

**698 → 710 is a second re-baseline in the same round, also not a finding.** The v0.7.0 fix wave
added 12 tests and removed none, all of them on decisions that previously had no executable
coverage at all: five for `language_action` (the reload's keep / swap / no-pack choice, which
could not be reached without a real `OcrEngine`), four for `startup_language`, one for
`freeze_rect`, and two for `from_config` seeding the two new settings. It also added one assertion
inside an existing test — `recogniser_available` on an upper-cased tag — which pins case folding
without moving the count. The clippy counts did **not** move here either: still 3 raw and 0 on the
bin target, at the same three sites.

**The Apply handler times itself** (`APPLY_BUDGET_MS`, `src/app.rs:93`) and prints
`chibipop: Apply took <n> ms (budget 50)` to **stderr** when it exceeds it. Nothing fails and no
test catches it — the cost lands on unrelated applications, because Apply runs on the thread that
owns `WH_MOUSE_LL` and `WH_KEYBOARD_LL`, and Windows drops a low-level hook that misses
`LowLevelHooksTimeout`. 50 ms is a 6× margin on that 300 ms, chosen to catch the regression long
before it can be felt. Read stderr after pressing Apply; a line there is the whole signal.

**The three accepted clippy errors — unchanged since 2026-08-09, sites re-confirmed 2026-08-11:**

| Lint | Site |
|---|---|
| `useless_conversion` — explicit `.into_iter()` in an `IntoIterator` argument | `src/lookup/deconj.rs:78` |
| `too_many_arguments` (8/7) | `src/lookup/model.rs:78` |
| `too_many_arguments` (10/7) | `src/ui/render.rs:699` |

It was **4** until 2026-08-09. The fourth was `while_let_loop`, on `worker_main`'s trigger drain;
the hot-reload branch replaced that loop with an explicit `drain` (a `Reload` message must never be
swallowed by newest-wins coalescing), so the lint went with it. That is a legitimate 4 → 3, not a
suppression — the count went **down** because the code did.

> [!warning] The clippy line changed on 2026-07-29, because the old one could not fail
> It used to be `grep -cE "^error: (doc list|explicit call|this function|this loop)"` —
> a count of **four hardcoded lint texts**. Any error from a *fifth* lint was invisible to it.
> That is not hypothetical: a test added that day introduced `cloned_ref_to_slice_refs` in the
> lib-test target, clippy reported **6** errors, and the gate still printed **5**. A gate that
> only counts the failures you already know about does not detect regressions.
>
> The replacement counts every `error` line and subtracts the two `could not compile … due to N
> previous errors` summaries. Also note **`--all-targets` means lib and lib-test are counted
> separately** — the summary lines said "5 previous errors" and "6 previous errors" for the same
> run, and only the second one was the truth.
>
> **Do not run `cargo clippy` twice in a row and trust the second number.** Cargo replays cached
> diagnostics inconsistently; back-to-back runs on an unchanged tree returned 0 then 5. `touch`
> a source file first, or take the first run's output.
>
> **In CI, pass `--color never`.** The workflow sets `CARGO_TERM_COLOR: always`, so cargo
> prefixes every diagnostic with an ANSI escape and `^error` matches nothing. The count comes
> back 0, the step fails with "expected 4, got 0", and the real clippy output right above it
> says 4. This gate had never once run before 2026-07-31 — the repo had no remote until then,
> so nobody had seen it fail.
>
> **Re-measure this baseline after any commit that could move it.** On 2026-07-31 the comment
> sweep took it 5 → 4, nobody updated the table, a later branch read 5 and it was called
> "unchanged" — and that 5 was quoted to a reviewer as the baseline, hiding a dead-code
> regression inside the one number whose whole job is to expose one. A baseline carried forward
> from memory is not a baseline.
>
> It happened again, better, on 2026-08-09: 4 → 3, because the hot-reload branch deleted the loop
> that produced the fourth. This time the drop was **predicted in the plan before it happened**
> ("Task 6 rewrites the very loop that produces accepted error #1, so the raw gate may legitimately
> drop to 3 — re-baseline, do not treat 3 as a violation"), which is what a re-baseline should look
> like: named in advance, so nobody has to decide at the gate whether a moving number is a
> regression. **A count that changes is only ever a finding or a re-baseline. Say which, in
> writing, in the same commit that moves it.**

**Why counts, not exit status.** The repo carries three accepted clippy errors; a plain
`-D warnings` run therefore always exits non-zero, and CI must assert the count is **3** rather than
that clippy passed. A 4th is a real regression — most often a field added by one commit and read by
the next, which is why a task that adds a field must be the task that reads it.

The bin target needs the accepted lints suppressed or clippy aborts before `main.rs` compiles:

```bash
cargo clippy --all-targets --all-features -- -D warnings \
  -A clippy::while_let_loop -A clippy::doc_lazy_continuation -A clippy::useless_conversion \
  -A clippy::too_many_arguments -A clippy::needless_lifetimes -A clippy::type_complexity 2>&1 | grep -cE "^(error|warning)"
```

**If anything under `src/dict/` changed, measure the rebuild's peak memory.** No test
catches this — it regressed to **19× the oracle's** and every test stayed green, because a
32 GB machine simply absorbs it. Needs the real archives, so it is not a CI check.

```powershell
$out = Join-Path $env:TEMP "mem_check.sqlite"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "C:\Users\Stella\chibipop\target\release\chibipop.exe"
$psi.Arguments = 'build-dict --library "C:\Users\Stella\Documents\dicts" --out "' + $out + '"'
$psi.RedirectStandardOutput = $true; $psi.UseShellExecute = $false
$p = [System.Diagnostics.Process]::Start($psi)
$peak = 0
while (-not $p.HasExited) { try { $p.Refresh(); $w = $p.WorkingSet64; if ($w -gt $peak) { $peak = $w } } catch {}; Start-Sleep -Milliseconds 100 }
$p.WaitForExit(); $p.StandardOutput.ReadToEnd() | Out-Null
Write-Output ("peak {0:N0} MB" -f ($peak/1MB))
```

| Measured 2026-07-29 | peak | elapsed |
|---|---|---|
| Rust, streaming (current) | **148 MB** | 33.7 s |
| Rust, materialised (the regression) | 9,641 MB | 83.3 s |
| Python oracle *(deleted 2026-07-31; kept for comparison)* | 498 MB | 83.9 s |

Anything over ~300 MB means the streaming was undone. **`PeakWorkingSet64` reads 0 once the
process has exited** — the peak must be sampled while it runs, which is why the loop above
exists. And `python` on this box is a **mise shim**: measuring it returns the launcher's 4 MB,
not the interpreter's. Use `AppData\Local\mise\installs\python\3.13.14\python.exe` directly.

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

### 1.7a Outlined glyphs still read at about half

```bash
./target/release/chibipop.exe probe --at <x>,<y> --region 820,60 --upscale 1
```

On **outlined** text — a thin dark contour around a white interior — expect roughly **50-55%**
of characters, and misreads that look confident. On ordinary solid text at the same size expect
**exact**. Measured 2026-08-08: 53.8% outlined vs 100.0% on the identical sentence in a solid
font. BACKLOG 8 carries the evidence and the two refuted fixes.

Score `ocr line 0:`, not `line:` — `line:` prints only when hit-scan resolves, so scoring off it
reports 0% for reads that succeeded.

**This is a known ceiling, not a defect.** If it ever starts reading exact, something changed —
find out what before celebrating.

### 1.8 Resources

```bash
ls -l target/release/chibipop.exe          # ~3.4 MB, limit 100 MB
```

| Measurement | Value |
|---|---|
| `run`, idle | 12 MB WS / 2.6 MB private, **0.000%** CPU |
| `watch`, 417 hovers, 3-pass | plateaus 37 MB WS / 15 MB private, handles flat at 209 |
| Startup to live tray icon | **0.18 s** |

⚠️ `run` under sustained **real** hovering has never been re-measured and recorded 94.8 MB at M3.
That is the one number still outstanding.

> [!warning] 1.9 through 1.13 were added 2026-08-09 and **have not been run**
> They are the acceptance checks for the resizable-capture / hot-reload branch, written from the
> code by an agent with no screen. Every one of them needs a human on the portrait secondary.
> Nothing below carries a ✅ and nothing below should be read as passing. They are also all
> *live-apply* checks, which is precisely the class no unit test can reach: the unit tests prove
> the new value is computed, never that a running instance started obeying it.

### 1.9 Settings apply without restarting

Record the PID (`Get-Process chibipop`), open Settings, change **Capture height**, press Apply.

- The **PID is unchanged** — that is the actual test of "no restart", not a proxy. Everything else
  in this entry is equally consistent with a fast restart; only the PID rules one out.
- The settings window **stays open**. It used to vanish, because the process died under it.
- A value under 80 is clamped and the status line says so. Bounds: height 80–600, width 100–1600.
- `probe --at <x>,<y>` reports the new region height.

> [!warning] `probe` is not a read-only observer, and it reads the file rather than the process
> **It writes.** `probe` calls `config::load_or_create`, whose NotFound arm **creates a fresh
> `chibipop.toml`** — including when `--region` makes the config irrelevant. That side effect is new
> as of this branch (`src/main.rs`, before the `--region` match). Probing a directory with no config
> is therefore not an inspection, it is an initialisation. Harmless in the ordinary case, since the
> app writes that file anyway — not harmless if you were about to conclude something from the file
> being absent.
>
> **It is a different process.** `probe` reads `chibipop.toml` from disk, so it proves Apply
> *persisted* the value and that a fresh process picks it up. It says nothing directly about the
> region the already-running instance is now using; that is what hovering, and the unchanged PID,
> are for.

### 1.10 Alphanumeric scanning

Uncheck **Scan alphanumeric text**, Apply.

- Hovering an English menu bar (`File`, `Edit`) produces **no popup**.
- 「3人」 still resolves **with the 3 intact** — hover the 人.
- Hovering the `3` itself produces nothing.

Segmentation is why this is a tier 1 check and not a unit test: it depends on how the live engine
splits that line. The filter makes a Latin-only word **unhoverable** rather than deleting it from
the assembled text, which is what keeps 「PCを使う」 looking up whole from the 使.

### 1.11 Trigger mode and both hotkeys apply live

Three bindings the settings window has always been able to edit, and that have never once taken
effect without a restart. The design's audit of "what needs recreating" missed all three.

In `chibipop run`, with the PID recorded:

1. Switch **Trigger** from `Live` to `Hold key`, Apply. Hovering alone must now do nothing; holding
   the trigger key while moving must raise the popup.
2. Press the **Trigger key** button, press a different key, Apply. The new key works, the old one
   does not.
3. Change the Anki group's **Shortcut key**, Apply, and add a card with the new key.

The observable in all three is the same: the new binding works **and the PID has not changed**.
Before this branch Apply restarted the process, so these appeared to work while the thing making
them work was the restart. Remove the restart and they silently stop — which is why a pass here is
meaningless without the PID beside it.

### 1.12 The scan overlay can be switched on live

Start `chibipop run` with **Outline what each hover captured** *off* (`[debug] show_scan_region =
false`, the shipped default). Turn it on in Settings, Apply, then hover.

- The capture outlines **appear**, with no restart.

**Start with it off.** Starting with it on tests nothing — that is the case that always worked.
Before this branch the overlay window was only *created* when the setting was on at startup, so
turning it on later had no window to show and the checkbox did nothing whatsoever. It is now
created unconditionally and merely shown on demand.

### 1.13 The capture guard tracks a live `exclude_from_capture` toggle

The one with real teeth: chibipop's own OCR capture must never contain chibipop's own popup.

With `chibipop run` live, uncheck **Hide the popup from screen capture**, Apply, then keep hovering
along a line of Japanese.

- Lookups stay correct — no popup text bleeds into them. Turn on **Outline what each hover
  captured** (1.12) and screenshot: nothing chibipop drew may be sitting inside a capture box.
  **A lookup that resolves the popup's own text is the failure.**
- Re-check the box, Apply, and it returns to the exclusion path — again with no restart.

Two mechanisms have to hand off cleanly. Exclusion on: Windows keeps the popup out of the capture.
Exclusion off: chibipop must instead hide the popup around each capture itself
(`capture_guard_active`). That flag is recomputed from what the windows **report after** the
affinity call rather than from what was asked for, because `SetWindowDisplayAffinity` is allowed to
refuse. Before this branch the cached value kept its startup setting for the life of the process,
so turning exclusion off left the guard off with it, and the popup contaminated the very lookup it
was displaying.

> [!note] 1.14 and 1.15 were added 2026-08-11 and **partially run the same day**
> They are the acceptance checks for the per-character-retrigger / OCR-language branch. Both are
> *live-apply* checks, the class no unit test can reach: the unit tests prove the new freeze rect
> and the new engine are **computed**, never that a running instance started obeying them.
>
> **Run on 2026-08-11, horizontal text, one machine (100% DPI, `ja` + `en-US` installed):**
> - **1.14 core — passes.** With the toggle on and mode Live, hovering 経 of 経験人数 showed 経
>   entries; moving one character right to 験 changed the popup to 験〔げん〕, freq 42368, without
>   leaving the line.
> - **1.15 — passes, both directions.** Switching Japanese → English (United States) → Japanese
>   left the **PID unchanged** each time (18080 throughout, identical start time), persisted to
>   `chibipop.toml`, and the engine genuinely swapped: with `en-US` active the same Japanese text
>   stopped resolving entirely, and resolved again on switching back.
> - The dropdown listed exactly the two installed recognizers by display name, and the corrected
>   caption rendered on one line, unclipped.
>
> **NOT exercised, and still owed — do not read these as passing:**
> - the **tategaki** case in 1.14, which is the path this branch broke and repaired;
> - that the toggle is inert in hold-key mode;
> - that drill-down and wheel-scroll still work with the toggle on, in either orientation;
> - 1.15's missing-recognizer path, and the startup fallback, which need a pack uninstalled.

### 1.14 Per-character retrigger

**The procedure spans two tabs, and that is not a mistake.** The checkbox **Look up each character
as you hover** is on the **OCR / Debug** tab; the **Trigger** radios that gate it are on
**General**. The checkbox is greyed out unless the trigger mode is Live, so setting this up means
moving between the two. (It was on General until 2026-08-11 — putting it there grew the window from
594 to 652 logical px on *every* tab, which at 150% scaling risked pushing the Apply row off the
bottom. See BACKLOG 11.)

With trigger mode **Live** (General) and **Look up each character as you hover** on (OCR / Debug),
hover the first character of a two-character word (経験) in **horizontal** text, then move one
character right **without leaving the line**. The popup must change to 験's entry. Turn the setting
off, press Apply, and repeat: the popup must now hold on 経験.

- In both states, moving onto the popup must hold it, and wheel-scroll and kanji drill-down must
  still work. That is the property the split freeze/reach rects exist to preserve.
- In hold-key mode the setting is inert **and the checkbox greys out**, by design. Switch Trigger
  to `Hold key` on General and watch the checkbox disable on OCR / Debug; the grey-out uses the
  same predicate as the back end, so a legacy `HoldShift` config greys correctly too.
- **The PID must not change** across either Apply. This setting is consumed on the pump thread in
  the `WM_TIMER` freeze check, and it applies to an **already-visible** popup the moment Apply
  lands — you do not need a fresh lookup to see it take effect.

**In vertical text, moving *down* the reading axis is expected NOT to re-fire the lookup.** Only
upward or lateral motion does. This is correct and deliberate. **Do not file it as a bug.**

The popup is placed directly below the anchor, so in tategaki the next glyph down sits exactly
where the corridor from the text to the popup has to run — the dead band *is* the next character,
and you cannot both retrigger on it and route a corridor through it. The corridor is therefore
built flush with the freeze rect (`sticky_region`, `src/geom.rs:96-100`), which puts the next
glyph's top band inside the corridor and its remainder underneath the popup. The alternative was
measured on this branch and is strictly worse: without the flush corridor, every downward move
re-fired the lookup and the replacement popup was **unreachable** — no scroll, no drill-down, no
Anki button — because it appeared one advance lower with the same gap beneath it. Reachability was
chosen over retriggering, deliberately.

The consequence to accept: with the toggle on, the feature is largely **inert along the primary
reading direction in vertical text**, and useful mainly in horizontal prose. What would actually
unlock tategaki is placing the popup to the *side* in vertical orientation. That is a separate
round and was consciously not taken here.

*(Horizontal text is unaffected by any of this. There, the corridor's source rect is identical to
the reach rect field-for-field, so the sticky region is byte-identical to v0.6.0's — and when the
toggle is off, freeze and reach are equal, which makes the whole three-rect array identical to
v0.6.0's for every user who does not opt in. "Default-off = default-unchanged" holds by
construction, not by convention.)*

### 1.15 OCR language

The **OCR language** dropdown is the first row of the **OCR / Debug** group.

Switch **OCR language**, press Apply, and confirm the **PID is unchanged** — that is the test of
"no restart", not a proxy — then hover the **same Japanese text you were resolving a moment ago**
and confirm it now resolves **nothing at all**. Switch back, Apply again, and confirm it resolves
again. That pair — Japanese stopping, then returning — is what discriminates "the engine really
swapped" from "the engine did not"; the unchanged PID alone does not, because a reload that
silently kept the old engine also leaves the PID alone.

> [!warning] Do not confirm the swap by hovering text in the **new** language — it can never resolve
> Lookup is an exact match on a Japanese headword against the `term` table
> (`src/lookup/sqlite.rs:53`, `WHERE surface = ?1`), and the shipped dictionaries are Japanese. So
> with `en-US` selected, the recognizer can read a line of English perfectly and **still raise no
> popup**, because there is no row to match — the OCR half succeeded and the lookup half had
> nothing to do. A positive resolve in the new language would require a dictionary **for that
> language**, which chibipop neither ships nor builds.
>
> This step used to read "hover text in the new language and confirm it resolves." It could only
> ever be failed, and the next person to clear this debt would have seen nothing and filed a false
> regression against a feature that was working. The 2026-08-11 run silently substituted the
> negative observable instead of following the step as written, which is how the defect surfaced.

- The dropdown lists the installed recognizers — the list comes from
  `OcrEngine::AvailableRecognizerLanguages()` — **plus the configured one when it is not among
  them**, appended as `<tag> (not installed)`. That row is deliberate: a hand-edited or
  since-uninstalled language stays visible and selectable instead of being silently reselected.
  **A first run with no Japanese pack shows `ja (not installed)`, and that is the warning.** The
  dropdown displays each language's **display name** while carrying its BCP-47 **tag** in a side
  table, so a display name can never reach `ocr.language` in the TOML — the appended row stores
  the bare tag, not the `(not installed)` label.
- If a language pack is removed while selected, lookups must keep working with the **previous**
  recognizer rather than breaking. The engine is rebuilt on the worker thread, and on failure the
  working engine is kept. **Losing OCR entirely is the failure this catches.** There are two
  distinct stderr lines and they mean different things (`src/text/ocr.rs:269` and `:277`, chosen
  by `language_action`):

  | Line | Means |
  |---|---|
  | `chibipop: no <tag> recogniser; keeping <tag>` | the tag is not in the installed list — the pack is gone |
  | `chibipop: <tag> recogniser failed, keeping <tag>: <err>` | it *is* installed but the engine would not build |

- **Read stderr, because success is silent and so is one class of regression.** An Apply whose
  language is unchanged returns early and prints nothing at all. That silence is fine here, but it
  means a future edit that reintroduced a hardcoded language at the reload site would be completely
  invisible whenever the hardcoded value happened to match — no message, no crash, no failing test.
  If you are testing this after touching the reload path, switch to a language you can *see* fail.
  The keep/swap/no-pack **decision** is now pure and unit-tested (`language_action`), so inverting
  that guard fails the suite; what is still unwitnessed by any test is the wiring around it — that
  `apply_settings` is handed the language the user picked, and that a swap really rebuilds.

---

## Tier 2 — mostly automatable (~5 min)

> [!important] Corrected 2026-07-31 — this tier is **not** human-only
> It said "none of this can be automated", because `SendInput` returned **0** and the call was
> rejected. That is true only **until the user grants input control**. Once they have, `SendInput`
> returns **1** and `WH_MOUSE_LL`/`WH_KEYBOARD_LL` fire normally — hold-shift mode was driven end
> to end on 2026-07-31 this way: Shift down + mouse move raised the popup, Shift up retracted it,
> reproducibly over two cycles.
>
> **Print `SendInput`'s return value first.** 0 = not permitted, ask the user. 1 = drive it.
>
> **And read the cursor back before believing anything.** `MOUSEEVENTF_ABSOLUTE` targets the
> **primary monitor** unless you also pass `MOUSEEVENTF_VIRTUALDESK` (`0x4000`) and normalise over
> the whole virtual desktop (`x * 65535 / (vw - 1)`; this box is 3640x1920). Asking for `2696,491`
> without it put the cursor at `1355,246` and looked exactly like a dead hook.
>
> Detecting the popup needs no screenshot: `EnumWindows` filtered by pid, then `GetClassName` —
> `ChibipopPopupClass` and `ChibipopOverlayClass` appear and disappear with it.

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
11c. **Press "Quit chibipop" in the settings window** → chibipop exits. ✅ **Confirmed by oniichan
    2026-07-29.**

> [!important] Corrected 2026-07-29 — this window **is** agent-verifiable
> This entry previously said the settings window's Win32 layer was unreachable from a tool
> shell, citing three failed mechanisms. That conclusion was **wrong**, and it cost real
> coverage — several rounds were reported as "unverifiable" when they were not.
>
> `FindWindowW` does fail, and now we know why: **window classes registered with
> `RegisterClassW` are process-local**, so another process cannot resolve the class *name* to
> an atom. `GetClassName` reads the string back perfectly well, which is why the class looked
> present and unfindable at the same time.
>
> What works, measured:
> ```
> FindWindowW('ChibipopSettingsClass', null)  ->  0           (the dead end)
> EnumWindows + GetWindowThreadProcessId==pid ->  hwnd, class 'ChibipopSettingsClass'
> EnumChildWindows(hwnd)                      ->  34 controls
> GetDlgCtrlID / GetWindowText                ->  id=117 'Add…', 118 'Remove', 119 ListBox, …
> PostMessageW(WM_COMMAND, id) / SendMessageW(LB_GETCOUNT)  ->  work cross-process
> ```
> So button presses, list contents and enable state can all be driven and asserted from a
> tool shell. **Only the visual result — wrapping, spacing, whether it looks right — still
> needs eyes.** Filter `EnumWindows` by PID; do not use `FindWindowW` on this class.

11d. **The console is hidden on a double-click.** Agent-verifiable by the same route, and
    confirmed 2026-07-29: launched without inheriting a console, `ConsoleWindowClass` exists
    with **`IsWindowVisible` = False** while `ChibipopSettingsClass` is True. That is
    `own_console()` hiding a console it owns alone. A visible black box here means
    `GetConsoleProcessList` returned something other than 1.
11b. **The Apply button's caption and hint.** Two processes open the same window and only one of
    them varies its caption. What the code does, as of 2026-08-09
    (`apply_caption`/`apply_hint`, `src/ui/settings_window.rs:772`):

| Opened by | Dictionary staged? | Caption | Hint |
|---|---|---|---|
| `chibipop run` | no | **Apply** | "Applying saves your settings and uses them right away." |
| `chibipop run` | yes | **Apply & Restart** | "Applying saves your settings and restarts chibipop." |
| `chibipop settings` | either | **Apply & Restart** | "Applying saves your settings and restarts chibipop." |

   The varying row is new with the hot-reload branch; `chibipop settings` is unchanged and does not
   vary. In the source the caption reads `"Apply && Restart"` — `&&` renders as one `&`, and a
   single `&` would render as an accelerator underline instead (see the traps table).

> [!warning] Corrected 2026-08-09 — this entry described behaviour that has never existed
> It asserted `chibipop settings` shows caption **"Apply"** plus a hint "Restart chibipop to use
> them", and blamed a `restarts` flag when they did not match. Three things were checked against
> the source, and all three came back against the entry:
>
> - That hint string appears **nowhere in `src/`**. It exists only in this branch's plan and design
>   spec, which are working notes and are not published with the repo.
> - There is **no `restarts` flag** anywhere in the codebase.
> - The caption has been a hardcoded "Apply & Restart" in **both** processes for its entire history.
>
> So **11b could not have passed on any commit, before this branch or after it** — it was a
> checklist item written from a design document rather than from the program, and every run that
> "passed" it passed it by not looking.
>
> The ruling was to **fix the doc, not the behaviour**: changing what `chibipop settings` says is a
> product decision, not a regression fix. The open question — that window says "Apply & Restart"
> and then restarts nothing — is recorded as **BACKLOG 9**.
>
> **The lesson is the general one.** A checklist item copied from a spec asserts what someone
> intended; only an item written against the program asserts what it does. When they disagree the
> spec is not automatically wrong, but the checklist is not evidence either way.
11e. **Close via the X (`WM_CLOSE`), not Escape or a button.** In `settings_only`
    (`chibipop settings`, or `chibipop run` before a dictionary exists) it fully exits
    chibipop, same as "Quit chibipop" — there is no tray to fall back on, so `wndproc`'s
    `WM_CLOSE` records the same `Cancel` outcome Escape does, and `settings_only`'s own match
    arm already treats `Cancel` and `Quit` alike. In a normal `chibipop run` (tray already up)
    it only destroys the settings window; hooks, popup and tray keep running. ✅
    **Agent-verified 2026-08-01**: `EnumWindows` filtered by pid, `PostMessageW(WM_CLOSE)` on
    `ChibipopSettingsClass`, `Get-Process` read after — `settings_only` exits with no
    stdout/stderr (a silent, successful return); `run` loses only `ChibipopSettingsClass` from
    the window list, with `ChibipopTrayOwnerClass` and the popup and overlay classes unchanged
    at the same pid. Suspected broken beforehand from reading `wndproc` alone: `WM_CLOSE` only
    ever records `Cancel`, which reads like "only Quit can end a windowless run" until you also
    read `settings_only`'s match arm. The two sites carry comments pointing at each other now.
12. **Reorder dictionaries → Apply** → order changes, and **`chibipop.toml` still holds the
    original substrings**, merely reordered. Invisible from the UI; check the file.
13. **Open Settings, touch nothing, Apply** → the TOML is unchanged apart from formatting, and it
    returns in well under a second. *(A settings-only Apply must never trigger a rebuild. Measured
    2026-07-29: 128 ms for `chibipop settings`.)*
14. **Add… → pick a `.zip` → Apply.**
    The acceptance run: import two term archives and one frequency archive into a clean `library/`,
    Apply, confirm the rebuild completes and lookups reflect all three; then remove one, Apply,
    confirm it is gone and the others still rank correctly.

> [!important] Corrected 2026-07-29 — the picker **is** agent-drivable
> This entry previously said `GetOpenFileNameW` could not be driven from a tool shell, citing
> three failed mechanisms. A fourth works, measured on 2026-07-29 and used to reproduce the
> staged-add defects end to end:
> ```
> EnumWindows by pid, class '#32770'          ->  the picker's hwnd
> GetDlgItem(dlg, 0x047C /* cmb13 */)         ->  the filename combo (created LATE - poll for it)
> SendMessageW(cmb13, WM_SETTEXT, 0, path)    ->  fills it, cross-process
> PostMessageW(dlg, WM_COMMAND, 1 /* IDOK */) ->  the dialog closes and the file is picked
> ```
> The trap that made it look impossible: the combo does not exist for the first ~1 s, and IDOK
> on an **empty** filename is a no-op that leaves the dialog open — which reads exactly like
> "IDOK is ignored". Wait for `GetDlgItem` to return non-zero before setting the text.
> Only the visual result still needs eyes.
14a. **Remove → Apply, with no picker involved.** ✅ **Agent-verified 2026-07-29** on
    `chibipop settings` against a fixture library, driven by `EnumWindows` + `PostMessage`:
    the row leaves the listbox and **`library/` is untouched until Apply**; Apply then moves that
    one archive into `library/.removed/`, writes `library.json`, rebuilds, swaps the database in,
    and only then deletes it. The database's `dict` table loses exactly that dictionary. During
    the build, `id=100` and `id=117` read `en=False` and the status line
    (`id=122`) shows `Rebuilding your dictionary…` then `Reading <name>…` — **never the child's own
    `wrote …chibipop.sqlite.tmp: 3 entries, 5 term rows`**.
14b. **Remove *everything* → Apply** → refused, in the window: `Not applied: that would leave
    chibipop with no dictionary`. ✅ **Agent-verified 2026-07-29** — the window stays open, the
    database's hash is unchanged and **no archive is deleted**, because the refusal comes before the
    first removal.
14c. **A corrupt `.zip` in `library/` → Apply** → `The rebuild failed. Your dictionary is
    unchanged.` ✅ **Agent-verified 2026-07-29** — window stays open, Apply re-enables, database
    hash unchanged, and stderr carries the innermost cause (`invalid Zip archive: Could not find
    EOCD`). **The library is put back too**: pressing Apply three times leaves one copy of each
    archive, not `extra (2).zip` and `extra (3).zip`.
14d. **Remove your last real dictionary while a corrupt `.zip`, or a frequency list filed under
    Dictionaries, is present** → refused, same message as 14b. ✅ **Agent-verified 2026-07-29** —
    the guard classifies by reading each archive, so neither can stand in for a dictionary. An
    unreadable file is listed in the Dictionaries box under its **file** name, so it can be
    selected and removed.
14e. **Two `chibipop settings` at once, each removing a different dictionary, both Apply** → one
    proceeds; the other is refused with `Not applied: another chibipop is changing your
    dictionaries - close it and try again`. ✅ **Agent-verified 2026-07-29**. Before the named
    mutex, both passed the guard and `library/` ended holding nothing but `library.json`.
14f. **Apply from `chibipop settings` while a `chibipop run` holds the database** → `Another
    chibipop is running. Close it, then Apply again.` ✅ **Agent-verified 2026-07-29** with the
    database held open without `FILE_SHARE_DELETE`: every archive is still there afterwards.

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
| **A task that adds a field must be the task that reads it** | `field never read` is a dead-code error, and the gate asserts an exact count — one extra breaks it. (Caught once as a 6th error against the 5-error gate of the day; the gate is 3 now, the trap is unchanged.) |
| **Ghost tray icons** | A force-killed instance leaves a corpse; right-clicking it does nothing. Sweep the cursor over the tray to reap them. |
| **Windows will not rename onto an open file** | A rebuild that ends in `Access is denied (os error 5)`. SQLite opens without `FILE_SHARE_DELETE`, so `chibipop run` cannot have the new database renamed over the one its worker is holding. It builds to `<out>.new` and swaps it in **after** the worker is joined, on the way to the restart. Measured, and pinned by `tests/rebuild.rs`. |
| **Never delete an archive before the rebuild proves out** | The user's `.zip` files are 50–200 MB downloads chibipop may not redistribute. Apply moves removals to `library/.removed/`, which `build-dict` cannot see because it scans top-level `*.zip` only, and deletes them only after the new database is in place. Every failure path calls `Pending::rollback`. |
| **"Which listbox is it in?" is not "is it a dictionary?"** | The builder decides by reading `index.json`. A frequency list filed under Dictionaries, or a corrupt `.zip`, once satisfied the "you would have no dictionary left" guard and got the last real one deleted. Ask `library::kind_of`. |
| **Nothing serialises two chibipops** | Both can read the library, both satisfy the guard, both delete a different archive. `lock::LibraryLock` (`CreateMutexW` + `ERROR_ALREADY_EXISTS`, named per library folder) is held for the whole Apply, rebuild included. |
| **`Option::take()` inside a tuple pattern always runs** | `if let (Some(a), Some(b)) = (poll(), flight.take())` takes on *every* poll, so the in-flight state is dropped before it is ever read. Cost a rebuild that reported nothing and never committed. Take only after the poll has matched. |
| **`cargo fmt` is not run here** | The repo has never been rustfmt-clean. Do not "fix" it. |
| **Stray files land in a broad `git add`** | Never `git add -u`/`-A`. Stage by name. |
| **A copy of `data/` beside the exe shadows the repo's** | `--dict` prefers beside-exe and only falls back to the working directory. A stray `target/release/data/` therefore wins silently, and keeps winning after the real data changes. Delete it rather than refreshing it. |
| **`AttachConsole` does not rebind std handles** | Cost a whole task, built and reverted. Measured: console subsystem redirects 647 bytes; GUI subsystem + `AttachConsole` redirects **0**; GUI + `SetStdHandle` redirects 0 **and panics** — in cmd as well as PowerShell. Test redirection in *both* native shells before believing a console change works. |
| **Returning TRUE from `CTRL_CLOSE_EVENT` does not claim it** | The handler runs, returns TRUE, and the system terminates the process anyway — it is documented that way. Do not "handle" the console's close box; delete the menu item with `GetSystemMenu` + `DeleteMenu(SC_CLOSE, MF_BYCOMMAND)`. |
| **Python's `json.dumps` is not compact; `serde_json` is** | Python defaults to `", "` and `": "` separators. Every row differs from a Rust port that emits compact JSON — spuriously on a whole-database diff, and for real in `entry.senses`. Hit twice. Match the separators, or the oracle diff drowns in noise. |
| **git-bash `/tmp` is not the `/tmp` a native tool sees** | `/tmp` maps to `AppData\Local\Temp` for bash but resolves to `C:\tmp` for python.exe or chibipop.exe — and `sqlite3.connect` *creates* the missing file, so the symptom is `no such table`, not `file not found`. Produced a false verification failure. Convert with `cygpath -w` before handing a path to a Windows program. |
| **`cargo test` prints five `test result:` lines** | Quoting the first reports roughly a third of the suite as the total. Sum them: `awk '/^test result: ok\./ {s+=$4} END {print s+0}'`. An agent got this wrong once and reported 245 for 248. |
| **A stripped dependency reads as a free one** | Task 3 measured 3.44 MB after adding `zip`, because nothing called it yet and the linker dropped it. Size a new dependency only once something actually reaches it. Also: 3,928,064 bytes is 3.75 MB, not 3.93 — divide by 2²⁰, not 10⁶. |

## When something fails

**Instrument, do not theorise.** The `ShowWindow` bug survived three confident hypotheses and fell
in one run of `eprintln!` printing the actual return value and window style. The general shape:
post the message by hand to isolate delivery from handling, log the Win32 return values, and only
then reason.
