# Architecture

This document is the map for an agent in this repository. It shows the structure,
the control flow, and the rules for each area. `CONTEXT.md` defines the domain terms.
`AGENTS.md` gives the commands and the workflow. The upstream pull requests contain
the architectural decisions.

## Project structure

```
src/                    Core lib `chibipop`: all behavior, no OS calls.
  controller.rs         Hover/popup state machine. Event in, Command out.
  worker.rs             Pipeline: capture, mask, OCR, lookup, present.
  text/                 mask.rs, layout.rs, frozen.rs, source.rs.
  lookup/               Deconjugation, SQLite queries, scoring, model.
  dict/                 Import, build, reindex, gloss, media, pitch.
  ui/layout/            Measured layout pass. Builds a PopupScene.
  analysis/             Japanese analysis over the committed IPADIC model.
  select/               Card selection state and the Gesture machine.
  config.rs settings.rs Shared Config and SettingsForm.
  present.rs            Unmeasured view model fed to the layout pass.
crates/chibipop-linux/src/    Linux bin: a calloop daemon.
  capture/ cursor/      Capture backends; cursor channel rungs.
  shortcuts/ control.rs Portal shortcuts session; control socket verbs.
  ocr/                  meikiocr over `ort`; models in ../models/meiki/.
  popup/                SCTK layer surface, tiny-skia, cosmic-text.
  settings/             The iced settings process and its rebuild path.
  tray/ lock.rs paths.rs  SNI tray, per-display flock, XDG paths.
crates/chibipop-windows/src/  Windows bin: a GetMessageW loop.
  input/ action/ plugin/  Hooks, the actions they drive, the plugin host.
  ui/render/            Direct2D paint, DirectWrite TextMeasure adapter.
tests/                  Core integration tests, fixtures, render sweep.
docs/                   REFERENCE, REGRESSION, RELEASING, LINUX, research.
themes/ plugins/ data/  CSS themes; bundled meikiocr; deconjugator.json.
  data/ipadic/          Committed IPADIC model, digest pins, and license files.
packaging/aur/ extras/  PKGBUILDs; desktop file, systemd unit, snippet.
tools/ scripts/         Benchmarks and censuses; package-linux.sh.
```

## Workspace and seams

The workspace has three crates. Core owns the behavior, and a platform bin owns the OS.
Both platform bins use `[[bin]] name = "chibipop"`. Therefore, one link command must
never build both binaries.

```
platform bin --Event--> Controller --Command--> platform bin
                            | Trigger
                            v
Worker: capture -> mask -> OCR -> lookup -> present --result--> Controller
```

- `Controller` and `Worker` live in core. A platform bin runs its native loop,
  synthesizes `Event` objects, and executes `Command` objects.
- Exactly three seam traits exist: `RegionCapture`, `OcrEngine`, and `TextMeasure`.
  A trait exists only where the implementation differs inside one binary.
- Input, tray, popup surface, paths, instance lock, and console handling are
  `Event`/`Command` variants or bin-local functions. They are never traits.
- All code is synchronous. It uses standard library threads and `mpsc`. It uses calloop
  for the Linux pump. D-Bus and portal clients run on their own threads. There is no
  async runtime.
- Core uses physical pixels only. The platform bin converts logical coordinates and
  fractional scale at the seam.
- Painting remains per-platform. Core provides the view model, the theme, and the
  layout math.

## Capture and masking

- Two Linux capture backends exist: wlr-screencopy v3 is primary, and portal ScreenCast
  with PipeWire is the fallback.
- The selection reads the advertised capability. It never reads the compositor identity.
  The `hyprctl cursorpos` cursor rung is the single permitted exception.
- Buffers use shm with CPU cropping only. No backend contains GPU plumbing.
- The app requests portal consent at startup. A denial never causes an exit. The hover
  path shows one actionable error with a retry action.
- The Worker masks chibipop's own on-screen rectangles in core before OCR. It applies
  a flat white fill with a hard edge.
- The mask boundary is a capture edge. The engine drops words that touch it. It never
  partially recognizes them.
- The build does not include the Windows hide-and-reshow capture guard on Linux.

## Input ladders

- `chibipop-linux` has two channel ladders. Both synthesize the same core `Event`
  objects that the Windows hooks synthesize.
- Cursor rungs: the ext-image-copy-capture cursor session, then portal
  `cursor_mode=METADATA`, then `hyprctl cursorpos` polling. A missing rung means the
  platform is unsupported. A startup diagnostic names the missing capability.
- Trigger rungs: the GlobalShortcuts portal, then a native compositor keybind into the
  control socket.
- The portal shortcut identifier set has exactly two members: `trigger` and `anki-add`.
- The system rejects evdev completely, even as a setting.
- `keyboard_interactivity: none` is a strict rule. The popup never takes focus.
- One control-socket verb exists for each global action. A verb exists only if a user
  can bind a key to it. No verb reads state, takes an argument, or composes.
- A portal press event and its native-bind verb use one code path.
  `shortcuts::action` maps the identifier and trigger mode to a `Verb`. In Toggle mode,
  it maps the trigger identifier to the `toggle` verb. `App::apply_verb` is the only target
  function.

## Popup and measurement

- `TextMeasure` takes an ordered list of styled spans and a wrap width. It returns
  per-line and per-span geometry and a baseline for each line.
- `TextMeasure` is permanently measure-only. No painting moves behind it.
- The inline layout pass remains a private module inside `src/ui/layout/`. Exactly one
  implementation exists, so it never becomes a trait.
- Both platform bins adapt to any measurer-contract change in the same commit.
- `TextMeasure` has exactly three methods: `measure`, `caret_boxes`, and `hit_offset`.
  `hit_offset` maps a run-relative point to a UTF-16 caret offset.
- `PopupScene` is the measured popup in one uniform, unit-agnostic pixel space. Core
  never receives a scale factor.
- The platform bin measures and places the popup. The Controller learns the result as
  `Event::PopupPlaced { rect }`.
- Comments in `crates/chibipop-linux/src/popup/surface.rs` and `popup/text.rs` describe
  the Wayland surface protocol rules.

## Selection

- Selection state lives in core as `select::Selections`. It stores one `CardSelection`
  per Card at the `all_cards` index and swaps with `swap_top`.
- `select::gesture::Gesture` owns click chains, drags, and deferred plain-click clear.
  Windows dispatch ticks provide its clock. Linux starts a temporary clock after pointer
  input and retires it after the click chain expires.
- A drag snaps to the unit of the press that started it: grapheme, word, configured
  triple-click unit, or Entry. A word drag snaps to graphemes until the analysis answers.
- Bins send `PointerDown`, `PointerMoved`, and `PointerUp` with a `TextAddr` from
  `PopupScene::text_hit`. A bin never decides a gesture.
- `gloss::select::sense_range` finds a Sense by shape, not by Dictionary. A Sense is an
  ordered-list item, an unordered-list item, a marked block, or a block that starts with
  a sense number. A nested glossary item wins over its explicit Sense ancestor. The
  per-Sense follower blocks join it. A marked plain-text line is also a Sense. Without a
  marker, the innermost block or newline-delimited line is the Sense. This fallback
  excludes sibling headword lines.
- `sense_core_range` stops a Sense before its first example. `line_range` selects the
  innermost block or one newline-delimited text line.
- `DocAddr` uses document order on role-visible leaves. A ruby node is atomic.
- `Selection::Ranges` prunes the Anki HTML and plain renderers. It keeps selected
  leaf bytes and valid ancestors.
- An active `CardSelection` overrides `first_dict_only` for the Anki glossary fields.
- Layout computes highlights and `Check` elements only when
  `SceneRequest::selection` is `Some`. Windows geometry goldens pass `None`, so they do
  not move.
- `popup.edge_autoscroll`, `anki.selection_buttons`, `anki.selection_separator`, and
  `anki.triple_click` configure selection. Theme `accent` supplies highlight and check color.

## Hover cadence

- Live mode paces by events with one-in-flight, latest-wins backpressure. No timer
  drives dispatch.
- It has no settle delay and no velocity gate.
- The damage-gated dwell re-check runs only while a popup is visible.
- At key press, trigger mode freezes one full grab of the output under the cursor
  before any popup exists. No capture and no mask run while the user holds the key.
- Release drops the frozen buffer, and each press captures again. The `toggle` command
  captures at toggle-on and holds the buffer until toggle-off. Toggle mode uses this path on
  both platforms: Linux sends the `toggle` verb, and Windows flips the hook latch.
- Every cadence number is a hardcoded constant. No cadence number is a setting.

## OCR engine

- meikiocr over `ort` is the only Linux `OcrEngine`. There is no runtime selection, no
  fallback, and no Python runtime.
- The Linux adapter never upscales crops. Windows uses an `UPSCALE` value of 2, and Linux
  uses 1. Measure again before you add upscaling to Linux.
- CI quality floor: horizontal CER <= 5 %, horizontal hit-scan >= 90 %, vertical CER
  <= 20 %, vertical hit-scan >= 75 %. It requires parity with the Python reference
  within 3 percentage points.
- The repository commits models under `crates/chibipop-linux/models/meiki/`. It pins
  their hashes against `SHA256SUMS.txt`. Two steps verify them: `scripts/package-linux.sh`
  when it stages the tarball, and `models::verify` when the engine starts.
- No first-run download path can exist. The app stays offline-first.
- The build statically links ONNX Runtime on linux-x64. It ships no shared object and
  sets no rpath.
- The source-AUR path keeps `--features system-onnxruntime` active. This feature opens
  the distribution library with dlopen.

## Japanese analysis

- `src/analysis/` is one concrete core module over Vibrato 0.5.2. It has no trait
  and no separate crate.
- `data/ipadic/system.dic` is the committed 47,788,814-byte IPADIC model. Its digest
  is pinned in `analysis::MODEL_SHA256` and `data/ipadic/SHA256SUMS.txt`.
- `data/ipadic/COPYING` and `data/ipadic/NOTICE` ship with the model in both packagings.
- One `std::thread` in `analysis::Service` loads the model lazily after the first paint.
  It analyzes only the top Card.
- Requests use latest-wins behavior. A stale generation is dropped.
- A load or digest failure falls back to UAX #29 word boundaries and emits one diagnostic.
- Word grouping merges auxiliaries, suffixes, verb suffixes, bound verbs, conjunctive
  particles, `する` after a verbal noun, and adjacent general or proper nouns. The unit
  for double-click is a full conjugation or a compound noun. Fine morphemes remain
  available for issue #52.
- The model has no download path. The application stays offline-first.

## Settings and config

- Linux settings run as a separate `chibipop settings` process with iced. The daemon
  contains no GUI toolkit.
- The shared `Config` and `SettingsForm` model lives in core. Both platform bins render
  widgets only.
- Live-apply saves the configuration and sends `reload` over the control socket. It never
  uses a structured push.
- `anki.include_dictionary_name` controls headings in both Anki glossary fields. The plain
  definitions use an HTML heading because square brackets can become furigana in Anki.
- Any setting that must round-trip is a field on the shared `Config`. It is never a
  platform-interpreted field, and never a `[linux]` side table.
- The configuration uses no sentinel values. An unresolvable `popup.font` value falls
  back to the platform default font with a visible warning.
- Each platform bin renders only the fields of its own platform. A save operation
  preserves the fields of the other platform without changes.
- The app derives autostart state by reading `~/.config/autostart/chibipop.desktop`.
  No TOML field stores this state.
- A dictionary rebuild writes beside the old database and atomically renames the new file
  over the old file.

## Dictionary and lookup

- The system derives the roles of a Dictionary by inspecting its banks. It never derives
  roles from a filename, and a user never declares them.
- Each role has an ordered, independently enabled list. Enabled state belongs to each
  role for each Dictionary.
- New imports go to the end of each relevant list in the enabled state.
- The identity of a Dictionary is its exact installed name.
- `term.freq` is a denormalized column. The hot lookup path reads it without a join.
- Reindex is an in-place SQL transaction that must never read an archive. It runs after
  every change to strategy, order, or enabled state.
- A Dictionary that does not report a word is not a data point. The system never
  substitutes `DEFAULT_FREQ` for this silence.
- The displayed frequency is always the number reported by the highest-ranked enabled
  Dictionary. It is never the computed rank.

## Verification

- Committed geometry-snapshot goldens verify layout changes under exact equality.
  The test allows no tolerance.
- Goldens change only through a human-reviewed `workflow_dispatch` bless run. CI has no
  push credentials and never auto-commits.
- Both test layers are permanent. Neither layer is deletable scaffolding.
- `fixtures()` and its documentation develop together. A name-pinning test enforces this
  rule.
- The CI image is pinned. An image bump is a scheduled baseline update.

**This coverage is asymmetric.** The golden layer pins real DirectWrite output on Windows
only. `src/ui/layout/tests.rs` runs against `FakeMeasure` and never calls cosmic-text.
Therefore, no committed golden pins real cosmic-text geometry.

Comments in `crates/chibipop-windows/tests/geometry_goldens.rs` and
`crates/chibipop-windows/src/ui/render/geometry.rs` describe the methodology.

## Platform integration

|Windows idiom|Linux equivalent|
|---|---|
|Notify-icon tray|ksni StatusNotifierItem over D-Bus|
|No autostart|`~/.config/autostart/chibipop.desktop`|
|Named per-session mutex|`flock` keyed per display in `$XDG_RUNTIME_DIR/chibipop/`|
|Beside-exe paths|Strict XDG mapping plus portable mode|
|`CREATE_NO_WINDOW`, show/hide console|No analogue: stderr plus a truncated logfile|
|Opt-in lookup log|The same gate, `debug.show_lookup_log`|

- Tray failure is never fatal on Linux. The app can operate completely without a tray.
- The instance lock and the control socket must use the same key.
- Library archives and the built database live under `$XDG_DATA_HOME`, not under cache.
  Therefore, a cache cleaner cannot delete them.
- The existence of one file determines whether the app uses portable mode or XDG mode.
  The app never reads both configurations.
- Lookup content reaches disk only when `debug.show_lookup_log` is active. Diagnostics
  always go to stderr and the logfile.

## Packaging and CI

- A release ships one tarball, `chibipop-vX.Y.Z-linux-x64.tar.gz`, and two AUR packages.
  `chibipop-bin` repacks the tarball, and `chibipop` builds from the release tag.
- The asset naming shape is a permanent contract. Every shipped binary parses asset names
  from `releases/latest`. Therefore, the `chibipop-v` prefix and the `-linux-x64.tar.gz`
  suffix must never change shape.
- On Linux, self-update is check-only and button-only. It never writes files and never
  connects to the network at startup.
- Only Windows uses the `.new` and `.old` executable swap.
- CI runs two native jobs with mirrored gates and no cross-compilation. Each job excludes
  the other platform bin crate.
- The CI runner image is pinned to `ubuntu-24.04`. This is the oldest image that can link
  the statically linked `ort` prebuilt binary.

**The runner pin and the `ort` linkage are coupled. Therefore, re-verify the runner floor
on every `ort` bump.** On 2026-08-26, developers found that the prebuilt binary is
statically linked on linux-x64. This finding raised the floor from `ubuntu-22.04` to
`ubuntu-24.04`. Ubuntu 22.04 still schedules jobs, but it cannot link
`__isoc23_strtoull` (glibc 2.38) or `_M_replace_cold` (GCC 13 libstdc++).
