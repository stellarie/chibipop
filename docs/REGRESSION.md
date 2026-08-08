# chibipop — regression checklist

Run this after any large change. It is ordered cheapest-first: **if a tier fails, stop and fix
before running the next one.**

Everything here was verified working on 2026-07-28, and tier 2 was re-confirmed on 2026-07-29. Numbers are what was actually measured on this
machine, not targets — a *different* number is not automatically a failure, but it is always worth
explaining before dismissing.

---

## Tier 0 — the automated gate (~2 min, no screen)

**This tier is the CI contract.** Every command below was re-run verbatim on 2026-07-29 and
reproduces the stated numbers, so it can be lifted into a workflow as-is. Two of them are
`grep -c` counts rather than exit codes, and that is deliberate — see the note under the table.

```bash
export PATH="/c/Users/Stella/scoop/persist/rustup/.cargo/bin:$PATH"; export RUSTUP_HOME=/c/Users/Stella/scoop/persist/rustup/.rustup
powershell -NoProfile -Command "Stop-Process -Name chibipop -Force -ErrorAction SilentlyContinue"
cargo test 2>&1 | awk '/^test result: ok\./ {s+=$4} END {print "TOTAL:", s+0}'
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | grep -E "^error" | grep -vc "could not compile"
cargo build --release 2>&1 | grep -E "^error|Finished"
```

| Check | Expected |
|---|---|
| Rust tests | **all green**, **626** total across **6** targets (was 416; re-measured 2026-08-08) |
| Clippy | **exactly 4** accepted errors (was 5; the comment sweep retired `doc list item`) |
| Bin-target clippy (below) | **0** |
| Release build | Finished, no errors |

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

**Why counts, not exit status.** The repo carries four accepted clippy errors; a plain
`-D warnings` run therefore always exits non-zero, and CI must assert the count is **4** rather than
that clippy passed. A 5th is a real regression — most often a field added by one commit and read by
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
11b. **`chibipop settings`** → the same window, captioned **"Apply"** and "Restart chibipop to use
    them" rather than "Apply & Restart". A caption mismatch means the `restarts` flag is wrong.
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
| **A task that adds a field must be the task that reads it** | `field never read` = a 6th clippy error against a 5-error gate. |
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
