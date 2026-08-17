# chibipop — regression checklist

Run this after any large change. It is ordered cheapest-first: **if a tier fails, stop and fix
before running the next one.**

Everything here was verified working on 2026-07-28, and tier 2 was re-confirmed on 2026-07-29. Numbers are what was actually measured on this
machine, not targets — a *different* number is not automatically a failure, but it is always worth
explaining before dismissing.

**§1.2, the match highlight, regressed on 2026-08-04 and was fixed on 2026-08-17** — found by this
page, filed as BACKLOG 29, fixed and re-verified the same day. The whole cycle is kept under that
heading rather than tidied away, because the useful artefact is not "it passes" but *how* a
green suite and a shipped release both missed it.

**Five exceptions to "verified", all marked in place.** Tier 1 items **1.9–1.13** were added
2026-08-09 with the resizable-capture / hot-reload branch and **have not been run**. Items
**1.14–1.16** were added 2026-08-11 with the per-character-retrigger / OCR-language branch:
**1.16 has not been run at all**, and 1.14 and 1.15 were **run only in part** the same day. What
passed, on one machine in horizontal text, was 1.14's retrigger with the toggle **on** and 1.15's
**switch path** — not all of 1.14, and not all of 1.15. Everything else in the three is still
owed, 1.14's toggle-OFF half and 1.15's missing-recognizer path among it. **The callout above 1.14
is the authority on the split; this sentence is a summary and is not the shorter list.** Item
**1.17** was added 2026-08-12 with the per-language-dictionary-lists branch and **rewritten
2026-08-13** for the two-box Dictionaries tab; it **has not been run at all in either form** — not
one clause of it. Item **1.18** was added 2026-08-13 with the rebuild-promotion branch, **has never
been run in any form**, and was **rewritten wholesale 2026-08-16 for v0.8.0**: the rebuild, the
staged `.new`, the promote and the two blocking joins it used to describe are all deleted, so the
old steps tested machinery that no longer exists. It is now the acceptance for a dictionary change
landing **in place, in seconds, with no restart** — and its **step 8, remove-and-add in one Apply,
is the single most load-bearing clause on this page**, because it is the only proof of a line that
no unit test can reach. Item **11b** was corrected 2026-08-09, having described behaviour that never
existed in any version of the program.

**A live pass was run on 2026-08-14, against v0.7.2, and it does not cover any of this.** Its
results are in `docs/superpowers/LIVE-PASS-2026-08-14.md` in the main checkout. Nothing in it
exercises the v0.8.0 incremental path, which did not exist that day.

> [!important] The machine has ONE monitor — corrected 2026-08-17
> The portrait secondary is gone. Windows reports one `\\.\DISPLAY1`, **2560×1080 at 96 DPI**.
> Ignore every "portrait secondary (x ≥ 2560)" instruction below, and the tier-2 callout's
> "3640×1920" virtual desktop. **Tier 1 now takes over the only screen there is** — say so before
> you start, and put the text up yourself.

> [!tip] The named corpus — `docs/fixtures/ocr-corpus.html`
> One kiosk page, every input class this page tests, at **known coordinates**: horizontal JA 26px,
> the same sentence outlined, alphanumeric-mixed, Simplified Chinese, Traditional Chinese, a second
> JA line, a wide-spaced line (suppresses the geometry merge — see 1.2), and a vertical column. It
> writes each block's rect and the per-character boxes of line J1 into `document.title`, so you can
> **predict a rect before you run**. Coordinates below assume it full-screen at 2560×1080.
> `docs/fixtures/scroll-test.html` is the companion for 1.7; it prints `window.scrollY` to its title.
>
> ```bash
> chrome --user-data-dir=/tmp/kiosk --no-first-run --kiosk file:///C:/Users/Stella/chibipop/docs/fixtures/ocr-corpus.html
> ```

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
| Rust tests | **all green**, **886** total across **7** targets, 2 ignored (873 → 893 → 885 → 886 on 2026-08-17; see below) |
| Clippy | **exactly 3** accepted errors (was 4; see below) |
| Bin-target clippy (below) | **0** |
| Release build | Finished, no errors |
| Apply handler | under **50 ms** (`LowLevelHooksTimeout` is 300 ms) |

**The test count is a floor, not an equality.** Adding a test must not break CI; a whole target
silently not running must. CI asserts `≥ 400` and prints the total; **873** is what this machine
measures today, so a *lower* number is the thing to explain. The clippy counts are equalities —
that is the difference between the two rows and it is deliberate.

> [!warning] One of those tests never runs here, and is counted as passed
> `golden_corpus` (`tests/golden.rs:31-34`) early-returns when `data/chibipop.sqlite` is absent,
> printing `SKIP golden_corpus: … not built` and asserting **nothing**. It is reported as `ok`, not
> as ignored, so **every total on this page includes one test that did not run** on any tree without
> a built database — which is every fresh clone and every worktree. The ignored tests beside the
> total are *different* tests. This is pre-existing and is recorded here rather than fixed: making the
> skip visible in the count means either failing the suite on a clone or teaching the `awk` to
> subtract, and both are their own change.

**A lower number is not automatically a finding either — it is a debt to explain.** The
772 → 794 entry below is the first on this page where a round *deleted* tests, and the honest
account of it is arithmetic, not reassurance: name what was removed, name what still covers the
behaviour, and show the subtraction.

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

**710 → 726 is a third re-baseline in the same round, also not a finding.** The BCP-47 tag-matching
fix added 16 tests and removed none: nine for `tag_matches` in `src/text/ocr.rs` (equality, case
folding, either side more specific, a differing script, symmetry, and the `zh-Han` vs `zh-Hans-CN`
boundary that a raw `starts_with` would have got wrong) and seven for `language_choices` /
`language_index` in `src/ui/settings_window.rs`, against a four-language installed list. The clippy
counts did **not** move here either: still 3 raw and 0 on the bin target, at the same three sites.

**726 → 729 is a fourth re-baseline in the same round, also not a finding.** The trailing-hyphen
guard in `extends_at_boundary` added three `tag_matches` tests and removed none: a trailing hyphen,
a lone hyphen against the empty string, and a leading hyphen. The first two were **red before the
guard and green after** — `tag_matches("ja", "ja-")` and `tag_matches("-", "")` both returned true
until it. The third was already green; it pins a case the guard does not change. The clippy counts
did **not** move here either: still 3 raw and 0 on the bin target, at the same three sites.

**729 → 730 is a fifth re-baseline in the same round, also not a finding.** The well-formedness
gate in `recogniser_available` added one test and removed none: a malformed tag is never available,
over nine shapes. It was **red before the gate and green after** — `recogniser_available("ja--JP")`
returned true via the installed `ja`, because the subtag-boundary guard only requires *a* byte after
the hyphen and `-` is a byte. That tag then reached `Language::CreateLanguage`, which rejects it, so
`run` returned `Err` with the console already hidden. The assertion is machine-independent:
well-formedness is checked before the installed list, so it holds on a runner with no recognisers.
The clippy counts did **not** move here either: still 3 raw and 0 on the bin target, at the same
three sites.

**730 → 767 is a re-baseline, not a finding**, and it is recorded here in the commit that moves it,
per the rule in the callout below. It is the first entry of a **new round**: the per-language
dictionary lists branch, five tasks and two fix rounds on top of the v0.7.0 release commit. It
added **37** tests and removed **none** — `git diff 5124d2d..9d477d7 -- src/` is **+37 `#[test]`, −0**,
which is exactly the gap, so no test was replaced by another and none was deleted. Counted from
that diff, the per-commit split is 3, 4, 4, 3, 0, 2, 8 across the five tasks and 4, 9 across the two
fix rounds; the one commit contributing 0 was a fix that changed `apply_to`'s behaviour under tests
that already existed. **The 0-contributing commit is the reason to read the diff rather than assume
one commit means one test.** The three runs above the table reported **767, 767, 767** — identical,
which is the point of running it three times — over six targets splitting 755 + 0 + 1 + 2 + 9 + 0,
with **0 failed** and the same **1 ignored** as before. The clippy counts did **not** move: still 3
raw and 0 on the bin target, at the same three sites.

**767 → 772 is the second entry of that round, and also not a finding.** The branch was reviewed
whole, and the fix wave that closed that review added **5** tests and removed none —
`git diff 9d477d7..HEAD -- src/` is **+5 `#[test]`, −0**. Three pin `scope_rows`' new
all-patterns-miss fallback — a stale list, a blank-only list, and a list naming only an unreadable
archive, each leaving every row searched, which is what the runtime does with the same input. One
pins that a second Apply rewrites the `per_language` key an earlier Apply wrote instead of dropping
it, and one pins that a list is not applied when its own recognizer is not the one running. The six
targets split **760 + 0 + 1 + 2 + 9 + 0**, with **0 failed** and the same **1 ignored**. The clippy
counts did **not** move: still 3 raw and 0 on the bin target, at the same three sites.

**772 → 794 is a re-baseline, not a finding — and it is the first entry here that goes *down*
before it goes up.** It is the first entry of a **new round**: the rebuild-promotion / two-box
Dictionaries branch, six tasks on top of the v0.7.1 merge (`4b3fe6a`). `git diff 4b3fe6a..HEAD --
src/` is **+33 `#[test]`, −11**, a net **+22** — which is exactly 794 − 772. Per commit the split
is 0, 1, 6, 3, 15, then **+8 −11** in the last one, so every deletion happened in one commit and
the arithmetic is `797 − 11 + 8 = 794`.

**The −11 is a demolition, not a regression.** All eleven tested `DICT_DIVIDER`, the fake row that
used to split the one dictionary listbox — nine under `// ---- the divider ----` and two under
`// ---- adding ----` — and the divider was deleted along with the tab that hosted it (§1.17). A
test whose subject no longer exists cannot be kept green honestly. What they asserted that
*outlives* the divider — the last searched dictionary will not cross out, an unreadable archive
does not count toward that rule, `Add…` lands among the searched ones — is re-asserted by the 15
pure move tests added one commit earlier, against two `Vec<String>` instead of one list and a
sentinel. `scope_rows`' five tests were untouched.

The three runs above the table reported **794, 794, 794** — identical, which is the point of
running it three times — over six targets splitting **782 + 0 + 1 + 2 + 9 + 0**, with **0 failed**
and the same **1 ignored**. The clippy counts did **not** move: still 3 raw and 0 on the bin
target, at the same three sites.

**794 → 873 is a re-baseline, not a finding — and it is the first entry here that goes up, then
down, then up again.** It is the v0.8.0 incremental-dictionary round, nine tasks on top of the
v0.7.2 merge (`036e4fd`). **The per-commit split is the truthful arithmetic and the whole-branch
diff is not**: `git diff 036e4fd..HEAD -- src/ tests/` reports `+82 −3` because a collapsed diff
pairs deleted `#[test]` lines against surviving ones. Counted per commit it is

`+3, +7, +10, +13, +17, +23, (+1 −13), +18` — **92 added, 13 removed, net +79**, and `794 + 79 = 873`.

Running total, one per task: **797, 804, 814, 827, 844, 867, 855, 873**. Task 9 moved it by **0**
(it renamed one test whose name the change made false and added two assertions to it).

**The −13 is a demolition, not a regression, and it is the whole point of the release.** Task 7
deleted the rebuild-and-promote path, and those thirteen tests went with their subjects: six over
`promote_outcome`'s pure state machine, three over the startup "leftover staged database" notice,
`stop_worker`'s, and the rest across `staging_path` / `PromoteDecision` / `Database`. **Every one of
them tested machinery that no longer exists** — there is no promote to decide about, no staged file
to notice, and no worker to stop. Keeping any of them green would have required keeping the code
that deadlocks. The one test added in the same commit covers what replaced them: the settings
window refusing a frequency-archive change. Note for anyone auditing that cut against the planning
documents — **the plan, the spec, the brief and the investigation all say "their four tests", and
there are six** over `promote_outcome` alone before the three nobody counted; deleting "the four"
leaves five compile errors.

The three runs above the table reported **873, 873, 873** — identical — over six targets splitting
**862 + 0 + 1 + 2 + 8 + 0**, with **0 failed** and the same **1 ignored**. The clippy counts did
**not** move across any of the nine tasks: still 3 raw and 0 on the bin target, at the same three
sites. That is worth stating plainly, because this round deleted roughly 500 lines of `src/app.rs`
and a whole module's worth of call sites: a demolition that leaves nothing orphaned shows up here as
a number that does not move, and `cargo check --all-targets` emitting **zero** warnings is the
evidence — `dead_code` fires on any kept item that lost its last caller, `unused_variables` on any
local orphaned by a deleted branch, and neither fired.

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

**873 → 893 → 885, both moves on 2026-08-17, neither a finding.**

- **+20** at `8388ef2`, the merge of PR #1: the HTML-glossary branch added tests and removed none.
  `src/dict/glossary.rs` is new, and `glosses_html` is pinned in `dict/build.rs`,
  `lookup/model.rs`, `present.rs` and `anki.rs`.
- **−10 +2** for the BACKLOG 29 fix: `merge_spaced_words` and its two helpers went, and their 10
  tests with them; 2 guards replaced them. 893 − 10 + 2 = **885**. The two `resolve_*_merges_*`
  tests were **rewritten, not deleted** — they now assert per-character geom *and* the gap-spanning
  property the deleted code claimed, so the behaviour keeps coverage from both sides.

Three runs at each number, identical each time; six targets splitting 874 + 0 + 1 + 2 + 8 + 0,
**0 failed**, the same **1 ignored**. Clippy did not move: **3** raw, **0** on the bin target.
`golden_corpus` ran rather than skipping, because this checkout has a built `data/chibipop.sqlite`.

**885 → 886 is a re-baseline, not a finding.** Task 2 of the plugin-system round deleted the
unreachable, single-pass `TextSource` trait and replaced it with `TextProvider`
(`src/text/provider.rs`), which `OcrTextSource` implements over the multi-pass
`resolve_at_tiled_scanned` and which `src/app.rs`'s `resolve_trigger` now calls through the
trait at both its call sites — the first design left the trait itself unreachable a second time,
caught before landing, and widened to carry `TextRead { resolved, scan }` rather than a bare
`TextSpan` so the one real call site could actually reach it. It added one test,
`text::provider::tests::a_provider_is_usable_as_a_trait_object`, and removed none. Repeated runs,
all **886**: seven targets splitting 875 + 0 + 1 + 2 + 0 + 8 + 0, **0 failed**, the same
**2 ignored**. Clippy did not move: **3** raw, **0** on the bin target — both counted fresh,
not assumed.

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

> [!warning] Redirect stdout to a FILE, never to a pipe — corrected 2026-08-17
> The version of this snippet that set `RedirectStandardOutput = $true` and called `ReadToEnd()`
> **after** the wait loop **deadlocks**: nothing drains the pipe while the child runs, the 4 KB
> buffer fills, and `build-dict` blocks forever on its own progress lines. The tell is a process at
> ~3% CPU with a WAL that stopped growing. It is corpus-dependent, which is what makes it nasty —
> a 768 k-entry build prints ~165 lines and squeaks through; a 1.13 M-entry build prints ~230 and
> hangs. Seen once, for 17 minutes, before anyone suspected the harness rather than the program.

```powershell
$out = Join-Path $env:TEMP "mem_check.sqlite"
$log = Join-Path $env:TEMP "mem_check.log"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath "C:\Users\Stella\chibipop\target\release\chibipop.exe" -PassThru -NoNewWindow `
     -ArgumentList @('build-dict', '--library', 'C:\Users\Stella\Documents\dicts', '--out', $out) `
     -RedirectStandardOutput $log -RedirectStandardError "$log.err"
$peak = 0
while (-not $p.HasExited) { try { $p.Refresh(); $w = $p.WorkingSet64; if ($w -gt $peak) { $peak = $w } } catch {}; Start-Sleep -Milliseconds 100 }
$p.WaitForExit(); $sw.Stop()
Write-Output ("peak {0:N0} MB in {1:N1} s" -f ($peak/1MB), $sw.Elapsed.TotalSeconds)
```

| Measured 2026-07-29 | peak | elapsed |
|---|---|---|
| Rust, streaming (current) | **148 MB** | 33.7 s |
| Rust, materialised (the regression) | 9,641 MB | 83.3 s |
| Python oracle *(deleted 2026-07-31; kept for comparison)* | 498 MB | 83.9 s |

**Re-measured 2026-08-17 at `8388ef2`, from scratch on a wiped output path.** Streaming still
holds — both peaks are far under the ~300 MB line, on a corpus half again as large as the original.

| Corpus | entries | term rows | peak | elapsed | db size |
|---|---|---|---|---|---|
| `Documents\dicts` — jitendex + JA freq + 大辞林 (the 2026-07-29 corpus) | 768,636 | 1,261,454 | **173 MB** | 35.2 s | **556 MB** |
| 6 archives — the above + 3 ZH-JA dictionaries | 1,129,265 | 1,982,693 | **198 MB** | 51.3 s | **851 MB** |

**The database roughly doubled, and `glosses_html` is why.** The pre-HTML build of 2026-08-01 is
**242 MB** at 774,087 entries; the post-HTML build above is **556 MB** at 768,636 — fewer entries,
2.3× the bytes. Measured, not inferred: over a 10,000-row sample of `entry.senses`, `glosses_html`
is **61.1%** of the payload, and all 10,000 carry a non-empty one. That is the price of the feature.
A `.sqlite` that no longer fits where it used to is now expected, not a mystery.

Anything over ~300 MB means the streaming was undone. **`PeakWorkingSet64` reads 0 once the
process has exited** — the peak must be sampled while it runs, which is why the loop above
exists. And `python` on this box is a **mise shim**: measuring it returns the launcher's 4 MB,
not the interpreter's. Use `AppData\Local\mise\installs\python\3.13.14\python.exe` directly.

**Run the suite 3× if anything touched a `static`.** Cargo runs tests in parallel threads of one
process, and a shared static produces an intermittent red that a single run will miss — this
happened once, with the wheel accumulator.

---

## Tier 1 — agent-verifiable, on real pixels (~5 min)

Needs text on screen. There is **one display** (2560×1080, 96 DPI) — put the corpus fixture up
full-screen and work against its published coordinates. `probe` reads a coordinate without moving
the pointer, so it disturbs nothing; the rows that *do* move the pointer are marked.

> [!note] Which language each command can actually test — added 2026-08-17
> **`probe` and `watch` hardcode `"ja"`** (`src/main.rs:140` and `:311`), so **no `probe` row on
> this page can test Chinese OCR at all**. Only `run` reads `ocr.language`. `watch` additionally
> ignores the configured capture size and always uses `CaptureSize::default()`. Anything about
> zh-Hans / zh-Hant therefore has to go through §1.21, which drives the real app.

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

> [!success] 1.2 regressed 2026-08-04, fixed 2026-08-17 — read before you touch geom
> **Passing, to the pixel.** 宿舎 gives `x=176 y=123 w=56 h=30`, which is what the word boxes
> predict. 図書館 `w=106`. 風邪をひいて `w=158`. A 1-char vertical match `x=2154 y=277 w=26 h=27`.
>
> **It failed for 13 days first.** `merge_spaced_words` folded single-character OCR words into one
> `TextGeom`, and Windows OCR emits one word per CJK character, so a whole line became one entry
> with `char_count = line length`. `union_chars` unions whole entries, so 宿舎 boxed
> `学生は宿舎に住んでいます。` entire — `w=314` where 56 was right. The height was correct
> throughout, which is what made it look plausible. The fix builds `span.geom` from the **unmerged**
> words; the merge and its two helpers are deleted. Full account in BACKLOG 29.
>
> Three lessons, in descending order of how much they will cost you again:
>
> 1. **Every geometry fixture used touching glyphs** — `x=100,130,160` at `w=30`, no gaps. Real OCR
>    emits gaps, and the merge only fired on gaps, so ten tests and a release saw nothing.
> 2. **`popup.highlight_match` is `false` in the live config.** A check whose output is off by
>    default is a check nobody runs.
> 3. **Suppress the suspect, do not argue with it.** The fixture's `letter-spacing:80px` line put
>    the gap over the merge threshold and the box came back exact. That took one probe and settled
>    what three plausible theories could not.

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

> [!note] 1.14–1.16 were added 2026-08-11; 1.14 and 1.15 ran **in part**, 1.16 **not at all**
> They are the acceptance checks for the per-character-retrigger / OCR-language branch. All three
> are *live-apply* checks, the class no unit test can reach: the unit tests prove the new freeze
> rect and the new engine are **computed**, never that a running instance started obeying them.
>
> **Read the pass list as the whole of what was covered.** Anything in 1.14–1.16 not named below as
> passing is still owed, whether or not the NOT-list bothers to repeat it. This callout was
> previously read the other way round — as if the NOT-list were the complete debt — which quietly
> promoted everything it forgot to mention.
>
> **Run on 2026-08-11, horizontal text, one machine (100% DPI, `ja` + `en-US` installed):**
> - **1.14's retrigger, with the toggle ON — passes.** With the toggle on and mode Live, hovering
>   経 of 経験人数 showed 経 entries; moving one character right to 験 changed the popup to
>   験〔げん〕, freq 42368, without leaving the line.
> - **1.14's no-restart property — passes, for the enabling Apply only.** The Apply that turned the
>   toggle on left the **PID unchanged** (18080, identical start time). That is a real observation:
>   1.14's PID bullet says "across either Apply" and this discharges one of the two. The other
>   Apply is in the NOT-list below, because it was never pressed.
> - **1.15's switch path — passes, both directions.** Switching Japanese → English (United States)
>   → Japanese left the **PID unchanged** each time (18080 throughout, identical start time),
>   persisted to `chibipop.toml`, and the engine genuinely swapped: with `en-US` active the same
>   Japanese text stopped resolving entirely, and resolved again on switching back.
> - The dropdown listed exactly the two installed recognizers by display name, and the corrected
>   caption rendered on one line, unclipped. The settings window measured 486×633, **unchanged from
>   v0.6.0** — which is what BACKLOG 11's shipped row requires, the checkbox having been moved to
>   OCR / Debug precisely so the window would not grow.
>
> **NOT exercised, and still owed — do not read these as passing:**
> - **1.14's toggle-OFF half was never performed.** The procedure's second half — "Turn the setting
>   off, press Apply, and repeat: the popup must now hold on 経験" — was not run. Nothing has
>   witnessed the toggle *stopping* the retrigger, only starting it, and that is the direction
>   "default-off = default-unchanged" actually rests on. It also carries the second of the two
>   Applies in 1.14's PID bullet.
> - **1.14's already-visible-popup property was not witnessed.** The run pressed Apply *first* and
>   hovered afterwards, so every popup it saw was raised under the new setting. The claim that it
>   "applies to an **already-visible** popup the moment Apply lands — you do not need a fresh lookup
>   to see it take effect" is therefore unverified. **The distinction matters:** a PID *was*
>   observed across that Apply and it *was* unchanged, so this is not a missing PID. What is missing
>   is the ordering — hover first, leave the popup on screen, *then* Apply, and watch that same
>   popup start retriggering without a fresh lookup.
> - the **tategaki** case in 1.14, which is the path this branch broke and repaired;
> - that the toggle is inert in hold-key mode, and that the checkbox greys out with it;
> - that drill-down and wheel-scroll still work with the toggle on, in either orientation;
> - 1.15's missing-recognizer path, and **the whole of 1.16** — both need a machine with a language
>   pack absent.

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
> (`src/lookup/sqlite.rs:53-54`, `WHERE surface = ?1`), and the shipped dictionaries are Japanese. So
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
- **Apply rewrites a hand-edited tag to the one Windows reports. That is expected — do not file
  it.** Since tags match by subtag boundary, `language = "zh-Hans"` selects the installed
  `zh-Hans-CN` row (`src/ui/settings_window.rs:1794-1797`); `read` then returns **the row's** tag,
  not the string that was configured (`:1999-2009`), `apply_to` copies it into `ocr.language`
  (`src/settings.rs:292`), and Apply writes the file with no compare-against-old
  (`src/app.rs:1162`). So pressing Apply **for any unrelated setting** rewrites `chibipop.toml` to
  `language = "zh-Hans-CN"`. A bare `zh` resolves the same way, to the **first** matching row in
  the order `AvailableRecognizerLanguages()` returns — nothing sorts that list — which was
  `zh-Hans-CN` on the 2026-08-11 machine. The file ends up holding the tag Windows actually
  reports, which is the intended self-healing. What *would* be a regression is the rewrite landing
  on a **different language** (`ja` becoming `ko`), or a tag being rewritten while OCR keeps using
  the old one.
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

### 1.16 The startup language fallback — **added 2026-08-11, not run**

**Nothing witnesses this path today.** `startup_language` is unit-tested as a pure function, and
its wiring at `src/app.rs:1557-1565` is not; the behaviour is described only in BACKLOG 13. It
decides whether a user whose language pack was uninstalled between runs gets a working app or a
silent no-op, so leaving it unexercised is not a small gap.

1. Quit chibipop (`Get-Process chibipop` returns nothing).
2. In `chibipop.toml` beside the exe, set `language` under `[ocr]` to a tag with **no pack
   installed on this machine** — `ko` on the 2026-08-11 machine, which had only `ja` and `en-US`.
   Confirm the tag really is absent first: 1.15's dropdown renders an uninstalled tag as
   `<tag> (not installed)`.
3. Launch **from a terminal**, not by double-click:

```bash
./target/release/chibipop.exe run
```

- **Expect on stderr, once, before the tray icon appears:**
  `chibipop: no ko OCR recogniser installed; starting with ja` (`src/app.rs:1561`).
- **Expect Japanese lookup to work.** That is the actual acceptance, not the message. Before this
  fallback, `OcrTextSource::new` returned `Err`, `run` bailed, and a double-clicked chibipop did
  **nothing at all** — the error went to a hidden console and a double-click launch never reaches
  the settings window, so there was no in-app way back.
- Restore the language afterwards, in Settings or in the file.

**Launching from a terminal is load-bearing, not convenience.** `main` calls `console::hide()`
unconditionally, but `own_console` returns a window only when this process is the console's *sole*
owner (`GetConsoleProcessList != 1` → `None`, `src/ui/console.rs:13-30`), so a shell's console is
left alone and stderr stays readable. A double-click owns its console and hides it — which is
precisely the condition that makes this whole class of failure invisible in normal use. BACKLOG 13,
limit 3: the fallback fixes "does nothing", not "says nothing".

- **This is a third message, and it is not either of 1.15's.** `installed; starting with` is the
  startup substitution; the two lines in 1.15's table are reload-path only. Confusing them means
  reporting on a path you did not test.
- **Settings will show the tag you configured, not the one that is running** — the dropdown reads
  `ko (not installed)` while OCR runs `ja`. `from_config` seeds it from `cfg.ocr.language`
  (`src/settings.rs:302`) and the substitution is a local in the worker thread that never writes
  the config back. Expected, not a failure.
- **A per-language dictionary list is not applied while the pack is missing** — the list belongs to
  the tag you configured, and the tag that is *running* is the fallback, so lookups search every
  dictionary by `display_order` instead. Deliberate as of 2026-08-12: filtering the fallback's
  Japanese hits through a list written for the missing language returns an **empty popup with no
  error at all**, which is worse than an unfiltered one. The main thread makes the same
  `startup_language` + `recogniser_available` call the worker does (`configured_recogniser_runs`,
  `src/app.rs:2379`), so the two cannot disagree about whether the pack is there. If the entry is
  for a language you can see results in, add it to `[dictionaries.per_language]` and confirm every
  dictionary still answers.
- **Not covered by this step:** a language that *is* listed but whose engine will not build. That
  still aborts startup exactly as before, and cannot be fixed without splitting
  `init_dpi_awareness` out of `OcrTextSource::new` — BACKLOG 13, limit 2.

### 1.17 Per-language dictionary lists — **added 2026-08-12, rewritten 2026-08-13 for the two boxes, not run**

**Nothing witnesses any of this today.** `resolve_dict_filter`, the box-to-box move
(`dict_move` / `dict_move_target`) and the which-box-is-acting decision (`acting_box`) are
unit-tested as pure functions; the Win32 layout, the selection tracking, every button path, and
that a *running* instance re-scopes its lookups are reachable only by hand. **Needs two installed
recognizers and at least two dictionaries** — with one of either, most of this entry is
unfalsifiable rather than passing.

**The tab is two listboxes as of 2026-08-13**, *Searched* above *Not searched*, with one
**Move up** / **Move down** pair that reorders inside a box and crosses between them at the
boundary. The single list split by a `──── not searched ────` row is gone, and so is the
`Include / exclude` button — **four** buttons only. Every step below is written against that
shape; a divider row appearing anywhere is a failure, not a stale checklist.

Set the first language's list to one dictionary and the second language's to the other, then switch
**OCR language** and press Apply.

- The **PID is unchanged** (`Get-Process chibipop`) — that is the test of "no restart", not a proxy.
- The Dictionaries tab re-scopes **as soon as the language dropdown changes**, before Apply: both
  boxes refill, the new language's list in *Searched* and the rest in *Not searched*. The caption
  above the top box reads `Searched — for the selected OCR language`; the one above the bottom box
  reads `Not searched`.
- Hovering the same word is answered by a **different dictionary set**. That is the acceptance; the
  tab agreeing with itself is not, and neither is the unchanged PID alone.
- Edit one language's list, switch language, switch back **without pressing Apply**: the edit is
  still there. Losing it is the failure this design exists to prevent.
- A language with **no** list still searches everything, exactly as v0.7.0 did.
- After Apply, `chibipop.toml` shows `[dictionaries.per_language]` with an entry per visited
  language. Each name is stored cut at its first `[` or `(`, so `Jitendex.org [2026-07-09]` must
  appear as `Jitendex.org`. **A surviving date stamp means the keying regressed**, and the entry
  will quietly stop matching the next time that dictionary is rebuilt. A title that contains no
  bracket (`大辞林　第四版`, `中日大辞典`) is stored whole and is **correct** — do not file it.

**Five checks that each cost a fix round, or a redesign. If time is short, run these.**

1. **The boundary crosses, and only at the boundary.** Click a middle row in *Searched* and press
   **Move up** / **Move down**: it reorders *within* the box and the highlight follows it. Click
   the **bottom** row of *Searched* and press **Move down**: it lands at the **top** of *Not
   searched*, highlighted there, and *Searched* loses its highlight. Click the **top** row of *Not
   searched* and press **Move up**: it lands at the **bottom** of *Searched*. At the far ends —
   top of *Searched* with **Move up**, bottom of *Not searched* with **Move down** — the button is
   **greyed**; press it anyway and nothing moves. Crossing is what the deleted `Include / exclude`
   button used to do; if a row cannot leave its box, the tab is back to being one list in two
   halves.
2. **The buttons follow the box you last selected in, not always the top one.** Select a row in
   *Not searched*, then press **Move up** — it must act on *that* row. A Win32 listbox keeps its
   selection when it loses focus, so both boxes normally hold one; if the buttons silently act on
   *Searched* regardless, the second box is decorative and every step above passes by accident.
   **Remove** obeys the same rule and must work from either box.
3. **The last searched dictionary cannot be moved out, and cannot be erased by Remove either.**
   With one row left in *Searched*, **Move down** greys out and stays greyed. With one readable row
   plus an **unreadable archive** (a corrupt `.zip`, listed under its file name) in the box, Move
   down on the readable one is **still greyed** — the rule counts readable names, not rows — while
   the unreadable archive itself can be moved down out of *Searched*. Now the other half of the
   same guard: **Remove** the last readable row instead, Apply, and confirm the language's entry in
   `chibipop.toml` **still names it** and has **not** become `[]`. **The stated reason for that
   rule was wrong until 2026-08-16 and is corrected here**, because the wrong one sends you hunting
   a symptom that cannot occur. `[]` and an absent key are *observationally identical* to both
   value-readers — `resolve_dict_filter` and `from_config` each `.filter(|l| !l.is_empty())`, and
   two existing tests already pin it — so removing an emptied key does **not** prevent "silently
   re-enables everything". Emptiness causes that either way, and it is unavoidable. The real reason
   is `is_scoped`, which asks `contains_key`: a stray `ja = []` counts as *having* a list, so newly
   dropped-in dictionaries are routed into the **excluded** column and the next Apply writes a real
   scoped entry — pinning an unscoped language behind the user's back. **That** is what an empty
   list costs, and it is why v0.8.0's removal path deletes an emptied key rather than writing `[]`.
4. **`Add…` appends to *Searched*.** With something sitting in *Not searched*, import a dictionary.
   The new row appears at the **bottom of *Searched***, selected and scrolled into view, and *Not
   searched* loses its highlight. Landing in the wrong box was a Critical — an import that
   silently went un-searched.
5. **A stale list degrades to searching everything — on both routes into it.** Quit, hand-edit the
   current language's entry to name a dictionary you have not installed (`ja = ["Daijirin"]`),
   start, open **Dictionaries**. **Every dictionary is in *Searched* and *Not searched* is empty**
   — and every one of them still answers hovers. The tab and the runtime must agree; the tab
   showing them all as excluded while all of them answered was a defect on this branch. **Then the
   second route, which is the one that was actually broken:** give the stale entry to the language
   you are *not* on, start, and switch **OCR language** to it on **OCR / Debug** before opening
   **Dictionaries**. Same expectation — a full *Searched* box and an empty *Not searched* one.
   The two routes run different code (`from_config` when the window opens, `scope_rows` on the
   switch) and only the first was guarded until 2026-08-12, so running the open route alone passes
   while the switch route is live. Step 3's aftermath is the same rule reached from a third
   direction: the removed dictionary's name is still in the list, matches nothing installed, and so
   everything answers again.

**The layout, which only eyes can check.** Each box shows **four full rows** without scrolling —
add a fifth dictionary and confirm a scrollbar appears rather than a row being half-drawn. Neither
caption is clipped (the *Searched* one is the longer; its tail, "OCR language", must be visible),
and the hint under both boxes reads whole on **one** line: `Order is matched by dictionary name.
Check both lists after a change.` (It said "after a rebuild" until 2026-08-16; Apply no longer
rebuilds anything.) **Apply and Quit must still be on screen at the tab's tallest**
— which needs *both* conditional warnings at once, the library-less notice and a stale order
entry. If both cannot be produced, check with one at 150% DPI: `BACKLOG.md` §11 records that case
as having roughly no headroom, and this tab is the one that governs the window's height when both
warnings show.

> [!note] Four things on this screen are expected — do not file any of them
> **Pressing Apply on step 5's screen rewrites the hand-edited entry** to the dictionaries actually
> installed, discarding the `Daijirin` you typed. Known limitation, not fixed in v0.7.1: configure
> the list *after* importing the dictionary. See `per_language` in
> [`REFERENCE.md`](REFERENCE.md).
>
> **Tabbing into a box does not retarget the buttons; selecting in it does.** Tracking follows a
> selection change, so Tab alone leaves the buttons pointed at the box you last clicked or arrowed
> in. The first arrow key inside the newly focused box retargets them.
>
> **Emptying the acting box greys all three of Move up, Move down and Remove** until you click a
> row somewhere. Nothing is selected anywhere at that moment, and that is the same behaviour the
> single list had when it emptied.
>
> **A language whose recognizer pack is missing ignores its list entirely** — the tab keeps showing
> it while every dictionary answers, because OCR is running the fallback language and not the one
> the list was written for. See §1.16 and `per_language` in [`REFERENCE.md`](REFERENCE.md). This is
> the one state where the tab deliberately does not match the runtime.

### 1.18 A dictionary change lands in seconds, without a restart — **rewritten 2026-08-16 for v0.8.0, not run**

**This is the acceptance for v0.8.0 and it has never been run in this form.** The entry it replaces
covered v0.7.2's rebuild-and-promote path: rebuild the **whole** database into
`data/chibipop.sqlite.new`, stop and **join** the worker, rename the staged file onto the live
database, respawn. **All of that machinery is deleted.** The join is why. The worker's closure
completed and `JoinHandle::join()` never returned, so the main thread — the one holding
`WH_MOUSE_LL` and `WH_KEYBOARD_LL` — pumped **zero** messages for the rest of the run and froze the
whole desktop. It did that twice. **v0.7.2 is deliberately never tagged**, because its headline
feature is the path that deadlocks.

v0.8.0 edits the live database instead. A removal deletes `term` → `entry` → `dict` in one
transaction; an addition parses that one archive and inserts with ids allocated from `MAX(…)+1`.
There is no staged file, no rename, no worker stop, no respawn and no restart — and **no worker
`join()` survives anywhere in the crate**, which is what makes the deadlock *unreachable* rather
than merely unvisited. Cost is proportional to the change instead of to the database: measured on
fixtures, ~105–400 ms to remove and ~240 ms per 50,000 entries added.

**What no test can reach, and why this entry exists.** The refreshed `DictInfo` list has to travel
`apply_edits` → `Reload` → `take_reload` → the worker's live copy, or a removed dictionary keeps
answering and a newly added one is never attributed. Every link *except the last* is unit-tested.
`take_reload`'s call site in `worker_main` needs a real `OcrTextSource` and a running message pump;
deleting that call kills **no** test. **Steps 5, 7 and 8 are the only proof that one line runs**, and
step 8 is the case that most reliably breaks it.

1. Start `chibipop run` **from a terminal**, so stderr is readable (§1.16 explains why a
   double-click hides it). **Record the PID** (`Get-Process chibipop`). Hover Japanese text and
   confirm a popup answers.
2. Settings → **Dictionaries** → `Add…` a dictionary that is not installed yet. Before pressing
   anything, read the hint under the button: it must say *Applying saves your settings and updates
   your dictionaries **in place***. **The word "rebuilds" appearing there is a failure** — that
   string said "rebuilds your dictionary" for the whole of v0.7.2, which stopped being true the
   moment Apply stopped rebuilding, and reading it before the press is the only way to catch it.
   The button reads **Apply**, not "Apply & Restart". Press it, and **time it with a clock, not by
   feel.**
3. The window goes busy, shows `Reading <name>…`, then a rising entry count, and ends on
   `Added <name>.` Two things to read here:
   - **Seconds, not minutes.** ~240 ms per 50,000 entries on a fixture; ~1.6 s extrapolated for
     大辞林 (334,751 entries). **A minute-long Apply fails this step even though nothing errored** —
     that wait is the whole reason the release exists.
   - **The counter starts near 1, not near the number of entries already in the database**
     (~360,000 on this machine). Progress carries the *absolute* `entry_id` and is rebased for
     display; an import that opens on "365,000 entries…" means the rebase was lost. Cosmetic, but
     it is the difference between "it is working" and "what is it doing?".
4. **The PID is unchanged and no window flickered.** Everything else in this entry is equally
   consistent with a fast restart; only the PID rules one out.
5. **Hover text only the new dictionary covers. It answers** — without reopening Settings and
   without a second Apply. Two separate things have to have happened for this: the worker's cached
   `DictInfo` list was refreshed, *and* the dictionary filter was re-resolved. Skip either and a
   freshly imported dictionary stays silent, or answers without attribution, until the next Apply.
6. **The popup never went dead.** Hover repeatedly **during** step 3's import, not only after it.
   The worker is never stopped and its read connection is never closed, so lookups must keep
   answering out of the existing rows for the whole edit. A gap where nothing answers means
   something is holding the reader, and it is a failure even if the import then succeeds.
7. **Remove it again** and confirm it **stops** answering — same hover, no popup or a popup with no
   entry from that dictionary — again with the PID unchanged and no restart.
8. **Remove one dictionary and add another in a single Apply. This is the critical case.** Do it as
   a replace-in-place if you can: remove `Jitendex [old]` and add `Jitendex [new]` in the same
   Apply. Both must take effect, the removed one must stop answering **and the added one must start**,
   and the status must name both (`Added …. Removed ….`). This is the one shape a stale cache cannot
   survive: ids are `MAX(…)+1`, so deleting the *highest* `dict_id` hands that same id straight back
   to the next insert (measured: 8 before removing dict 7, 4 after). A worker still holding the old
   list will then attribute the new dictionary as the old one, or answer from a dictionary that no
   longer exists. **No unit test reaches this**, and it is the exact symptom class this entry was
   originally written for.
9. **Apply both a dictionary and something else at once** — stage an import *and* change the popup
   font or a hotkey in the same Apply. **Both** must take effect. The v0.7.2 restart was what
   carried the non-dictionary half; removing it nearly dropped those changes silently.
10. `library/.removed/` is **gone** (a successful Apply deletes the quarantined archives and the
    folder), the removed dictionary's `.zip` is no longer in `library/`, and the added one is.
    `data/chibipop.sqlite` carries a new mtime. **There is no `.new` file at any point** — nothing
    in the app writes one any more; see the callout below about the one already on this machine.
11. **Report the `Apply took N ms` line.** The Apply handler times itself (`APPLY_BUDGET_MS`,
    `src/app.rs`) and prints `chibipop: Apply took <n> ms (budget 50)` to stderr **only when it
    exceeds 50 ms**. On v0.8.0 the UI thread does no database work at all — it reads the form,
    checks for a frequency archive, takes the library lock and spawns — so the expected result is
    **no line**. Say either way in the report: the number if it appears, "no line" if it does not.
    Nothing fails on this and no test catches it; the cost lands on unrelated applications, because
    Apply runs on the thread that owns the low-level hooks.
12. Quit from the tray: it exits within about a second. **The shutdown `stop_worker`/join is
    deleted**, so this is no longer the second place the deadlock could be reached — but run it in
    the same session as the Apply anyway, not from a fresh launch.
13. Read stderr across the whole run for `hide was not acknowledged`. A lookup already inside the
    capture guard when Apply lands can still print it once, and that one is expected.
14. **The whole desktop stays responsive.** Keep the mouse moving continuously **for the whole of
    step 3's Apply** and again **through step 12's quit**, watching the cursor rather than
    chibipop's window: it must never stutter, jump or crawl, and typing in another window must stay
    instant. This is the check that the two 2026-08-13 desktop freezes are gone at the root. Both
    ran with `WH_MOUSE_LL` and `WH_KEYBOARD_LL` still installed on a main thread that had stopped
    pumping, which serialises **every mouse move and keystroke on the entire desktop** behind
    chibipop for up to `LowLevelHooksTimeout` — 300 ms by default — per event. **Watch for the other
    shape of it too.** Windows may answer a hook that misses its timeout by dropping it rather than
    waiting again (see the 2026-07-27 spike finding), in which case the symptom is not a slow
    desktop but hover going quietly dead after the Apply with nothing on stderr. Either one fails
    this step, and **step 6 is what catches the second**.
15. **The frequency refusal, which has been unit-tested as a string and never seen.** Stage a
    frequency archive with `Add…` and press Apply. Expect **nothing to move** — no file leaves
    `library/`, no config is saved, the staged list stays in the form so you can drop the frequency
    zip and Apply the rest — and the status box to read, **on more than one line**, that frequency
    lists rank the words in every dictionary so changing one needs the whole database rebuilt, then
    the literal command with your real paths substituted:
    `chibipop build-dict --library "<lib>" --out "<db>"`. **The line break is the check.** The box
    is a Win32 multiline `EDIT`, where a bare `\n` does not break a line; if the command runs into
    the prose on one line, the CRLF was lost and the user cannot read the command.

> [!important] The `.new` left on this machine is now inert — delete it
> `C:\Users\Stella\chibipop\data\chibipop.sqlite.new` is **133,390,336 bytes** and still sits beside
> the live 242 MB `chibipop.sqlite`. Until v0.8.0, startup printed a stderr line naming it. **That
> notice is deleted along with the staged-file lifecycle, so nothing will ever mention that file
> again and no code path will ever adopt or overwrite it.** It is inert and harmless, and it is
> 133 MB of nothing. **You can delete it.** The previous version of this entry told you to expect a
> stderr line about it at step 1 and to treat the file surviving as normal; both of those
> instructions are now wrong, which is why they are gone rather than softened.
>
> Beside it on the same machine: `chibipop.old.sqlite` (243 MB) and
> `chibipop.degraded-2026-07-29.sqlite` (133 MB). Neither is referenced by any code path either.

> [!note] A full rebuild is still needed for four things — none of them are dictionary changes
> `chibipop build-dict --library <dir> --out <db>` survives and all four of its jobs were verified
> against the real release binary on 2026-08-14: **first run** (no database at all), **schema
> migration** (a file at `schema_version` 1 is refused by the reader until it is rebuilt),
> **corruption** ("file is not a database"), and **repair / drift**. **Frequency changes are the
> fifth, and new in v0.8.0** — see step 15. A frequency list ranks words across *every* dictionary,
> so adding or removing one means re-ranking `term.freq` across the whole `term` table — every
> dictionary at once — rather than inserting one archive. The settings window refuses it rather
> than pretending.

> [!note] Three things here are expected — do not file any of them
> **A settings window that is already open is not re-rendered after an Apply.** Its stale-order
> warning and its library-empty notice are whatever they were when it opened. The list rows already
> show what you just typed, so this is cosmetic.
>
> **After a *partial* failure the Dictionaries list is briefly a lie.** A failed add clears from the
> staged list along with the successful ones, so a dictionary that did not import stays listed until
> you reopen the window. The status names it (`Not applied: …`), and the library and the database
> are consistent with each other; only the form is stale. Clearing selectively needs a form change
> and not clearing at all is worse — a second Apply would re-import the ones that succeeded.
>
> **A popup left on screen from before the change still shows the old answer** until the next
> hover. Nothing repaints it in place.

### 1.19 The database can now drift from the library, and says so — **added 2026-08-16, not run**

**Editing in place permanently breaks an invariant the app relied on since M2:** that
`data/chibipop.sqlite` *is* `build-dict(library/)`. It no longer is, and cannot be made to be
without the multi-minute wait v0.8.0 exists to remove. `meta.source_hashes` records which archives
built the database and both the add and the remove path keep it current, so a mismatch is
**detectable** — and detection is all this is. **It must never start a rebuild by itself**; doing
that automatically, on window open, would be the worst possible surprise.

The comparison is unit-tested against both encoders. The notice reaching a real status box is not.

1. With chibipop **stopped**, drop a term `.zip` straight into `library/` by hand — not through
   `Add…`, which would import *and* apply it.
2. Start `chibipop run` and open Settings. The status box says your library and your dictionary
   database no longer match, names the archive under **In your library but not in the database**,
   states that nothing is broken and lookups still work, and gives the command with your real paths
   substituted: `chibipop build-dict --library "<lib>" --out "<db>"`. As in §1.18 step 15, **the
   command must be on its own line** — the box is a Win32 `EDIT` and a bare `\n` would not break it.
3. **Nothing rebuilds.** No progress, no busy window, no child process, and the database's mtime is
   unchanged when you close the window.
4. Now the other direction: stop chibipop, move that archive back out of `library/`, and reopen
   Settings. The archive is named under **In the database but no longer in your library** instead.
5. Open Settings **from the tray** as well as at startup — both routes check, and only the first
   was wired at first.
6. **Three cases that must produce no notice at all**, each of which was a false alarm caught
   during the build: a library with **no term archive** in it (a rebuild is impossible, so offering
   one is worse than silence); a library holding a **corrupt/unreadable** `.zip` (those are never
   recorded in `source_hashes`, so one broken download would otherwise pin a permanent, unfixable
   notice); and a database whose `meta.source_hashes` is **absent or unparseable** — that is a
   legacy or hand-built file and it reports nothing.

> [!warning] `cargo run` will always claim drift on this machine — do not file it
> `library_dir()` resolves **beside the executable**, so a development tree pairs the real 242 MB
> `data/chibipop.sqlite` with an empty `target/debug/library/`. Run the check against a shipped
> layout, or an installed copy, not `cargo run`.

### 1.20 `chibipop settings` still rebuilds, and now fails generically — **added 2026-08-16, not run**

The standalone settings window (`chibipop settings`, and the path `run` falls into when there is no
database at all) is **deliberately untouched by v0.8.0**: there is no worker, no hooks and no open
database, and a first run genuinely does need a full build. It keeps its rebuild — but it no longer
stages a `.new`, it builds straight onto the database (with `rebuild::run`'s own `<out>.tmp` →
rename still in front of it).

**One user-visible consequence, and it is a loss of specificity.** Apply a dictionary change from
`chibipop settings` **while a live `chibipop run` holds the database open**. The build now fails at
its own rename, so the status reads the generic `The rebuild failed. Your dictionary is unchanged.`
where v0.7.2 read `Another chibipop is running. Close it, then Apply again.` The dictionary archives
are still rolled back, the database is still untouched, and hovers in the live instance still
answer from it — only the sentence is less helpful. **Confirm the rollback, not just the message:**
the staged archive is back in `library/`, `library/.removed/` is empty or gone, and
`data/chibipop.sqlite` has its old mtime.

---

### 1.21 All three OCR languages resolve — **added 2026-08-17, run**

`probe` cannot do this (it hardcodes `"ja"`). Drive the real app: set `ocr.language`, start `run`,
put the pointer on the matching line of the fixture, screenshot the popup. One restart per
language — a config **file** edit is not the live-Apply path, §1.9 is.

| `ocr.language` | fixture line | hover at | expect |
|---|---|---|---|
| `ja` | J1 `y=120` | `191,136` | 宿舎 / しゅくしゃ / freq 21663, Jitendex + 大辞林 sections |
| `zh-Hans-CN` | ZS `y=430` | `165,447` | 学习 / **xué·xí**, 中日大辞典　第二版 only |
| `zh-Hant-TW` | ZT `y=530` | `165,547` | 學習 / **xuéxí**, 小学館中日辞典 第3版 only |

All three passed on 2026-08-17. The Chinese rows also **prove §1.17, the per-language dictionary
lists**, from the other side: with `zh-*` selected the JA dictionaries are absent from the popup.
That is the visible half of the feature, and it was unrun until now.

Check the installed recognisers first — a missing one is not a chibipop failure:

```bash
powershell -NoProfile -Command "\$env:PSModulePath='C:\Windows\system32\WindowsPowerShell\v1.0\Modules'; [void][Windows.Media.Ocr.OcrEngine,Windows.Foundation,ContentType=WindowsRuntime]; [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object { \$_.LanguageTag }"
```

This box has `en-US`, `ja`, `zh-Hans-CN`, `zh-Hant-TW`. **PowerShell 7 cannot load the WinRT type**
— it must be Windows PowerShell 5.1, with `PSModulePath` reset or the PS7 paths leak in.

### 1.22 The Anki card carries HTML, if the field map asks for it — **added 2026-08-17, run**

The `[[anki.field_map]]` `source` values are `expression`, `reading`, `glossary`, `glossary_html`,
`frequency` (`src/anki.rs:228-234`). **`glossary` is plain text; `glossary_html` is the formatted
one.** They are different fields, and picking the wrong one fails silently.

Mined 2026-08-17 into a sample deck, 12 notes over three languages, `glossary_html` mapped:

| Language | Words | Result |
|---|---|---|
| `ja` | 宿舎 · **風邪をひく** · 図書館 · 辞書 · ランク · **猫** | 148–3349 chars of HTML, freq present |
| `zh-Hans-CN` | 学习 · 中文 · 简体字 | 111–565 chars, no freq (the freq dict is JA-only) |
| `zh-Hant-TW` | 學習 · 繁體字 · 例句 | 456–1530 chars of HTML, no freq |

Two of those rows carry their own proof. **風邪をひく was mined by hovering 風邪をひいて**, so the
card holds the dictionary form and deconjugation reaches Anki. **猫 came off the vertical column**
with `prefer_vertical = true`, which flips the capture to 100×500.

The HTML is real structure: `<ul><li>` glosses, a `forms` `<table>` of 図書館 / としょかん /
ずしょかん, bold and superscript spans, and `<a href="?query=図書">` cross-references.

> [!warning] Two things to expect, neither a defect
> **The live config maps the plain source.** `target/release/chibipop.toml` has
> `source = "glossary"`, so real cards get plain text from a build that can produce HTML.
>
> **Some dictionaries have no markup to render.** 中日大辞典　第二版 stores plain strings, so
> `glosses_html` falls back to plain — correctly. It falls back with **literal newlines**, and an
> Anki field is HTML, so those cards render as one run-on paragraph. 学习 shows this; 繁體字, from
> 小学館中日辞典, does not. Converting the newlines to `<br>` on the fallback would fix it.

**Adding the same expression twice fails, and says so** — the popup shows `✗ Failed to add`. That is
the duplicate guard, not a broken hotkey. Hover a different word to re-test.

**Clean up after this row:** `deleteNotes`, then `deleteDecks` with `cardsToo`, then confirm
`deckNames` is back to what it was. Use a scratch deck, never a real one.

> [!note] Two passes on 2026-08-17 — one 2560×1080 display, the corpus fixture
> **Pass 1**, at `8388ef2`. Passed: 1.1, 1.3, 1.5, 1.6, 1.7, 1.21, 1.22. **Failed: 1.2.**
> **Pass 2**, after the BACKLOG 29 fix, to prove it rather than assume it. **1.2 passes**, predicted
> before running and matched exactly. Everything from pass 1 re-run and unchanged, with the boxes
> corrected: 1.3 `w=273 → 158`, 1.5 identical anchor *and* box across four points, 1.6b vertical
> 1-char `h=185 → 27`, the alnum run `w=203 → 78`. **1.6a still resolves nothing at 500×100** — the
> known-broken case must stay broken, and does. Tier 0 re-gated at **885 ×3**, clippy 3 / 0.
>
> **1.7a is not comparable, and that is the finding.** Outlined **60.9%** vs solid **91.3%** here,
> against the recorded 53.8% / 100.0%. The gap reproduces and the misreads are confident
> (ひ → `乙、`), but this is a different font at a different size, so the percentages are not the
> same measurement. `--upscale 1` also costs the solid line its two dakuten (で→て, だ→た); the same
> line reads exact at the default region. **Score a font against itself, in one run, or not at all.**
>
> **Still unrun:** 1.9–1.20 as marked, and 1.8's sustained-hover memory figure.
>
> **Artefact worth knowing:** `probe` reads whatever is on screen, chibipop's own popup included.
> One probe came back reading `xué・Xi` off the popup covering the line. Dismiss it before you probe
> underneath.

### 1.23 Real text inside the PNG-encode bracket — **added 2026-08-17, not run**

**Why this exists.** The plugin system sends captures to a plugin as base64 PNG. `tests/png_cost.rs`
measured the encode against three synthetic buffers and produced a **bracket**, not an answer:

| Buffer | Bytes | p95 |
|---|---|---|
| `uniform`, best case | 1,163 | **4.98 ms** |
| `text_like`, two-tone | 2,423 | 4.93 ms |
| `noisy`, worst case | 600,516 | **21.09 ms** |

About **4.7 ms is fixed WinRT overhead**, independent of content. Real screen text sits somewhere
between 4.98 and 21.09 ms, and no synthetic buffer can say where. **Only a real capture can.**

The work proceeded on the ruling that the encode is paid only by plugin users, whose engines run
100–300 ms anyway. This step is what confirms or withdraws that.

**Run it while you are here for any other tier 1 item.** It needs no separate session.

**Steps**

1. Put `docs/fixtures/ocr-corpus.html` full-screen at 2560×1080.
2. Capture the JA line J1 region with `probe --at <x>,<y> --region 500,100 --dump`.
3. Encode that real buffer through `encode_png` and record the byte count and p95.

**Pass** when the real byte count and p95 land inside the bracket above, and the p95 is **under
10 ms**. Record the number here either way.

**Fail** — meaning over 10 ms — is not a defect in this checklist. It is the signal to reopen spec
section 6 and consider the length-prefixed binary frame named in spec section 12.

### 1.24 Provider trait, no behaviour change — **added 2026-08-17, not run**

**Provider trait, no behaviour change.** Hover a word on `docs/fixtures/ocr-corpus.html` line J1.
The popup text, the resolved word and the highlight rect must match what the same hover produced
before this branch. Record the rect. `union_chars` on 宿舎 measured `x=176 y=123 w=56 h=30` on
2026-08-17.

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
> the whole virtual desktop (`x * 65535 / (vw - 1)`). ~~this box is 3640x1920~~ — **stale as of
> 2026-08-17: the box is 2560×1080, one display.** Asking for `2696,491` without the flag once put
> the cursor at `1355,246` and looked exactly like a dead hook; with one monitor that coordinate no
> longer exists at all.
>
> **Simpler, and it worked on 2026-08-17:** plain `SetCursorPos` from a PowerShell tool call drives
> the hover end to end — pointer onto the word, popup up, `SendKeys 'a'` fires the Anki hotkey and
> the card lands. No `SendInput` normalisation, no virtual-desktop arithmetic. Nudge the cursor
> twice (`191,136` then `192,137`); a single `SetCursorPos` onto a stationary point may not raise
> the popup on its own.
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
    them promises a restart. What the code does, as of 2026-08-13
    (`apply_caption` `src/ui/settings_window.rs:954`, `apply_hint` `:959`):

| Opened by | Dictionary staged? | Caption | Hint |
|---|---|---|---|
| `chibipop run` | no | **Apply** | "Applying saves your settings and uses them right away." |
| `chibipop run` | yes | **Apply** | "Applying saves your settings and rebuilds your dictionary." |
| `chibipop settings` | either | **Apply & Restart** | "Applying saves your settings and restarts chibipop." |

   **Row 2 changed on 2026-08-13** and this table moved with it, which is the whole lesson of the
   correction below. `chibipop run` used to read **Apply & Restart** with a restart hint when a
   dictionary was staged; it no longer restarts, so it no longer says so. The caption now varies
   only by `ApplyMode` — `apply_caption` lost its `staged` parameter entirely — while the hint
   still varies by both. `chibipop settings` is untouched and still hands off to a fresh process.
   In the source that caption reads `"Apply && Restart"` — `&&` renders as one `&`, and a single
   `&` would render as an accelerator underline instead (see the traps table).

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
| **Windows will not rename onto an open file** | A rebuild that ends in `Access is denied (os error 5)`. SQLite opens without `FILE_SHARE_DELETE`, so a `build-dict` cannot have its output renamed over a database another process is holding. **This is why v0.8.0 stopped renaming.** Dictionary changes now edit the live database through a second read-write connection in WAL mode; nothing is staged and nothing is renamed, so the trap is not on that path at all. It is still live for the two paths that do build a whole file: `chibipop build-dict` from a terminal, and `chibipop settings` (§1.20) — both fail at the rename against an open database, which is why every "quit chibipop first" instruction in the app says so. **The v0.7.2 answer to this trap — stage a `.new`, stop and *join* the worker, rename, respawn — is deleted**, because that join is what deadlocked the main thread and froze the desktop. |
| **`join()`ing a thread from the thread that owns the input hooks is a desktop-wide freeze** | Not a rebuild problem; a Win32 one, and the reason v0.7.2 is never tagged. `WH_MOUSE_LL` and `WH_KEYBOARD_LL` are serviced by their owning thread's message pump, and Windows serialises **every mouse move and keystroke on the machine** behind a hook that is not answering. A blocking `join()` on that thread stops the pump. Worse, the worker's own teardown needed that pump — its closure completed and `join()` never returned. Two whole-desktop freezes. **The fix was to delete the path, not the join**, so `join()` is now reachable from nowhere on the worker; see `docs/BACKLOG.md` §24 before reintroducing one. |
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
| **Merging OCR words merges their geometry too** | *(The code that did this is gone — the trap is not.)* A match highlight that is the right height and the wrong width, the whole line, at every match length. Any per-character claim about geometry dies the moment words are merged upstream of it, and `union_chars` can only union whole entries. Before adding a merge for text, ask what it does to geom. Suppress it (wide letter-spacing) to tell this apart from a broken union. |
| **Every geometry fixture used touching glyphs** | A whole class of bug that ten tests and a release walk straight past. Real OCR emits *gaps*; a fixture at `x=100,130,160` with `w=30` has none, and gap-conditional code is invisible to it. When a code path branches on spacing, at least one fixture must be spaced. |
| **`glossary` and `glossary_html` are different Anki sources** | Cards that look plain next to a popup that looks rich. Nothing errors — the field map just asked for the other one. Check `target/release/chibipop.toml` before believing the feature is missing. |
| **A child that prints progress will fill a 4 KB pipe and stop** | A build at ~3% CPU with a WAL that stopped growing, forever. `RedirectStandardOutput = $true` + `ReadToEnd()` after the wait loop is the trap; redirect to a **file** instead. Corpus-dependent, so it passes on the small corpus and hangs on the big one. |
| **`probe` and `watch` are Japanese-only** | A Chinese OCR check that "fails" while the app works. Both hardcode `"ja"` (`src/main.rs:140`, `:311`); only `run` reads `ocr.language`. `watch` also ignores the configured capture size. |
| **DXGI declines a region that crosses a screen edge** | `DXGI capture unavailable (no DXGI output for region); using BitBlt` on any probe within half a capture-width of x=0. Not a failure — the fallback is the design — but it means edge-of-screen probes are not measuring the DXGI path. |
| **A stripped dependency reads as a free one** | Task 3 measured 3.44 MB after adding `zip`, because nothing called it yet and the linker dropped it. Size a new dependency only once something actually reaches it. Also: 3,928,064 bytes is 3.75 MB, not 3.93 — divide by 2²⁰, not 10⁶. |

## When something fails

**Instrument, do not theorise.** The `ShowWindow` bug survived three confident hypotheses and fell
in one run of `eprintln!` printing the actual return value and window style. The general shape:
post the message by hand to isolate delivery from handling, log the Win32 return values, and only
then reason.
