# M3 acceptance results and the first real memory measurement

Branch `feat/m3-popup`. Binary: `target/release/chibipop.exe`, confirmed current against
HEAD (`cargo build --release` reported `Finished ... in 0.14s` — nothing to recompile).

## 1. Memory measurement

**Verdict up front, not buried: the 50MB hard bar and the 20MB target are BOTH MISSED under
real use.** A freshly-started, never-touched process is comfortably under budget; the
brief's own prescribed condition — "use it normally" — is not. Both are reported below;
neither is softened.

### Method used to generate the workload, and why two methods were needed

The screen constraint for this task forbids driving the OS cursor (the user's physical mouse
overrides it). That makes two of the brief's three readings impossible to generate against
the real, interactively-driven `chibipop run` process without inventing a fake workload. Two
different, honestly-labelled methods were used instead:

- **The real `chibipop.exe run` process** was measured at genuine startup and after several
  minutes of *zero generated interaction* — both conditions need no cursor at all, so these
  numbers are real, direct measurements of the production binary, not a proxy.
- **A throwaway harness** (`examples/mem_probe.rs`, deleted before this commit — confirmed by
  `git status` below) drove `OcrTextSource::resolve_at` → `LookupEngine::run` →
  `present::build` directly, in one long-lived process, over 20 fixed points on real Japanese
  text. The text was shown in a borderless WinForms window on the **secondary** monitor,
  positioned and populated entirely through .NET calls (`Location`, `Size`, `Label.Text`) —
  no cursor movement anywhere. The 20 points were not guessed: two calibration
  `chibipop probe --at X,Y` calls against that window returned the exact on-screen bounding
  box of every recognised character, and 20 of those real boxes' centres became the harness's
  fixed points. Each of the 20 iterations still does a fresh `BitBlt` capture, a fresh
  `Windows.Media.Ocr` recognise call, a fresh SQLite lookup and a fresh `present::build` —
  the only things skipped are the popup window, the D2D/DirectWrite renderer, the tray and
  the input hooks. **This means the harness's numbers are NOT a measurement of
  `chibipop run` itself** — they isolate whether the resolve/lookup/present pipeline alone
  grows with repeated use, separate from the D2D paint path. Both result sets are reported
  below, clearly attributed, never merged into one misleading number.

An operational snag, disclosed rather than silently worked around: the on-disk
`target/release/chibipop.toml` (gitignored, not source-controlled) predated Task 10's
`exclude_from_capture` config field and made the *current* binary refuse to start
("missing field `exclude_from_capture`"), confirmed by actually running it before assuming.
Deleted so `load_or_create` regenerated current defaults — equivalent to a fresh install,
not a change to any tracked file.

### Readings — the real `chibipop.exe run` process (PID 39432)

| When | WorkingSet | PrivateBytes | Handles | Threads |
|---|---|---|---|---|
| Startup (T+5–7s, 3 identical samples) | 11.2 MB | 2.52 MB | 194 | 5 |
| T+33s, zero interaction | 11.46 MB | 2.42 MB | 194 | 2 |
| **Idle, T+330s (~5.5 min), zero interaction** (3 identical samples) | **11.5 MB** | **2.5 MB** | 194 | 4 |
| **After a forced working-set trim** (`SetProcessWorkingSetSize(-1,-1)`, returned `True`) | **0.05 MB** | **2.5 MB** | 194 | 4 |

No cursor input, synthetic or real, was generated against this process. I cannot rule out
that the user's own physical mouse incidentally crossed Japanese text somewhere on his
primary monitor during the 5.5-minute window — chibipop does not log successful hovers
(by design, decision 4) — but the numbers show no sign of it: startup and the 5.5-minute
idle reading are within 0.3 MB of each other on both metrics.

**The working-set trim is the most informative single reading.** Working set collapsed from
11.5 MB to 0.05 MB — essentially all of it was reclaimable pages the OS simply hadn't bothered
to reclaim yet — while PrivateBytes, the actually-committed memory, did not move at all
(2.5 MB before and after). This is a direct, controlled demonstration of exactly the
distinction the brief asked about: WorkingSet64 overstates what this process genuinely holds
at rest; PrivateBytes is the trustworthy number here.

### Readings — throwaway pipeline harness (`mem_probe.exe`, PID 26644, separate process)

| When | WorkingSet | PrivateBytes | Handles | Threads |
|---|---|---|---|---|
| Pre-construction | 2.48 MB | 0.43 MB | 59 | 1 |
| Post-construction, pre-loop (stable, 4 samples / 3s) | 9.85 MB | 2.01 MB | 149 | 4 |
| Mid-loop (~lookup 4–5 of 20) | 22.5 MB | 8.14 MB | 202 | 5 |
| **After all 20 lookups** (20/20 resolved+hit, 0 errors) | **26.05 MB** | **8.37 MB** | 203 | 5 |
| +20s idle after the loop (30 identical samples) | 26.05 MB | 8.37 MB | 203 | 5 |

Construction (OCR engine handle, SQLite open with the 256MB mmap pragma, rule load,
`dicts()`) costs a modest ~7.4 MB WS / ~1.6 MB private on its own. The bigger jump —
another ~16 MB WS / ~6.4 MB private — appears within the first handful of the 20 *real*
lookups, then **stays perfectly flat for the remaining lookups and for 20 full seconds of
idle afterward**, despite those 20 lookups covering 20 different characters across 4
different sentences (different dictionary entries, different gloss text, different
presentation content each time). A genuine per-lookup leak would show continued growth
across all 20 calls and during the idle tail; this shows one initialization-shaped jump and
then nothing.

### Previously recorded, cited not re-derived (Tasks 7/8, `docs/superpowers/sdd` progress ledger)

During oniichan's own real, sustained use of an earlier M3 build:

| When | WorkingSet | PrivateBytes | Handles | Threads |
|---|---|---|---|---|
| First sample | 75.0 MB | — | — | — |
| Minutes later | 94.8 MB | 60.0 MB | 848 | 19 |

Both figures exceed the 50MB hard bar. This could not be regenerated this task — it requires
real, varied, sustained hovering against the actual D2D-painting process, which the screen
constraint rules out — so it is cited as the operative "ordinary use" evidence, not softened
or replaced by this task's much smaller cold-start numbers.

### Floor or slope?

**For the resolve → lookup → present pipeline specifically: FLOOR, and this task now has
direct, controlled evidence for it**, not just an inference: one jump on first real use
(construction cheap, first OCR/lookup call expensive), then completely flat — both across
the remaining lookups and across 20 idle seconds afterward. The real process's own
startup-vs-5.5-minute-idle comparison (11.2 → 11.5 MB) adds the same signal: no drift from
wall-clock time alone.

**For the full application (D2D/DirectWrite paint included): consistent with a floor, but
NOT proven flat — this is the one honest gap left.** Tasks 7/8's two real-use samples were
**still rising** (75.0 → 94.8 MB) when last observed, not yet plateaued. That is compatible
with a *slower* one-time warm-up than the OCR/lookup pipeline's (D2D device/font/glyph
caches filling in as new glyphs are painted, still bounded) — but it does not rule out an
actual per-hover leak somewhere in the paint/`show_at`/`measure` path, because nobody has
yet taken a third real-use sample several minutes after the second to see whether it
plateaus or keeps going. **This task could not take that sample** — it requires real cursor
movement over varied Japanese text, which the screen constraint forbids. That is the
single measurement that would close this gap: three or more WorkingSet/PrivateBytes samples,
spaced minutes apart, during one continuous real session, watching specifically whether
PrivateBytes plateaus or keeps climbing.

The handle/thread counts sharpen this further rather than settle it: my own readings never
exceeded 203 handles / 5 threads (harness) or 194 handles / 5 threads (real process, at
rest), but Tasks 7/8's real-use sample recorded **848 handles and 19 threads** — 4.3x and
~4x higher respectively. Handle growth under real, sustained use this large is itself worth
tracking independently of raw bytes; a handle leak can precede a byte-visible one.

### Likely culprits — named, not guessed at as a fix

Per the spec's own risk table and this task's data:
- The **construction-time floor** (~7–11 MB) is the OCR engine handle + `RoInitialize`, the
  SQLite connection with its 256MB mmap pragma configured (mapping address space is cheap;
  pages fault in on touch, unmeasured until touched), rule loading, and (real process only)
  the D2D device/factory, DirectWrite factory, tray icon and input hooks.
- The **first-use jump**, proven in isolation for OCR/lookup/present (+16 MB WS / +6.4 MB
  private), is most likely `Windows.Media.Ocr`'s recognition model/pipeline data being paged
  in on its first real `RecognizeAsync` call — a common lazy-load pattern — plus real SQLite
  b-tree pages for the specific queried terms faulting in through the mmap.
- The **additional cost visible only in the full app's real-use numbers** (75–94.8 MB WS,
  60 MB private, vs. this task's 26 MB WS / 8.4 MB private for a same-shaped workload minus
  D2D) is most likely the Direct2D device + DirectWrite factory + glyph/font-cache warm-up
  on first (and possibly repeated, as new glyphs appear) popup paint. This is the one
  component this task could not isolate or measure directly — confirming it needs the
  real-use, multi-sample follow-up named above, not a guess at what to optimise.

## 2. Manual acceptance checklist

Several of these need the user's own eyes and mouse. Marked clearly; nothing below is
claimed as confirmed unless it genuinely was, by a stated method.

| Item | Status | Evidence / method |
|---|---|---|
| Popup beside the hovered character, correct definition, both modes | **OUTSTANDING — needs oniichan** | Capture exclusion blocks this task's own screenshots of the real popup (same wall Tasks 5/7 hit). The underlying resolve→lookup pipeline was re-confirmed correct (20/20 successful, real on-screen text, this task), but glyph-level on-screen legibility is explicitly not something I can verify myself. |
| Both trigger modes (Live / Hold-Shift) | **Live: verified indirectly (default config, ran this session). Hold-Shift: OUTSTANDING — needs oniichan.** | Hold-Shift has never been empirically witnessed live since Task 6's own report flagged it outstanding; still not closed. |
| Focus never stolen | Verified at the **mechanism** level only | `SWP_NOACTIVATE` proven in the M3 win32/D2D spike across `SW_SHOW`/`SW_SHOWNOACTIVATE`/`SetWindowPos+SWP_NOACTIVATE`; `window.rs` unchanged since. Not re-exercised against a real focus-sensitive app this task (would need real hovering). |
| Popup does not appear in its own OCR capture | **Verified** (cited, not re-derived) | Task 1: `accepted=true`, 0/307,200 px. Task 10: guarded capture measured 63% → 0%. Startup message reconfirmed this session ("capture exclusion active..."). |
| Flips rather than crossing a screen edge | **Verified — automated** | `geom.rs`'s 12,201-placement sweep (Task 3); reconfirmed green this session (`cargo test --release`, 103/103). This is the automated check, not a fresh live visual confirmation. |
| Tray switches mode and quits cleanly | **Icon add/remove: verified by Task 8** (toolbar button-count technique). **Real menu click (open, radio bullet, dismiss, mode switch, Quit): OUTSTANDING — needs oniichan.** | This task's own shutdown used a non-forced `taskkill`, not a menu click — see below; it exercises the process-exit path, not the menu. |

## 3. Shutdown method used for this task's measurement process

Non-forced `taskkill /PID 39432` (same precedent as Task 8): `taskkill` reported
`SUCCESS: Sent termination signal`, and the process was gone within 2 seconds — no need to
escalate to a forced kill. **Disclosed rather than assumed:** `ui/window.rs`'s `wndproc`
only handles `WM_PAINT` explicitly and falls through to `DefWindowProcW` for everything
else, so it is not established here whether this delivered a message that ran the app's own
four-step `Drop`-based shutdown (which is what a real tray "Quit" click triggers via
`PostQuitMessage` inside the message loop) or a harder OS-level termination that skipped it.
Windows tears down the low-level input hooks on any process exit regardless (Task 8's own
finding); whether the tray icon was removed via `Drop` or is a lingering shell "ghost" icon
was not verified either way this task — an attempt to reuse Task 8's own tray-toolbar-count
technique hit a `ToolbarWindow32`-not-found wall on this box and was abandoned rather than
spend further time on a check the brief did not require.

## Files

- `examples/mem_probe.rs` — throwaway, deleted before this commit (`git status` confirms).
- This note.
