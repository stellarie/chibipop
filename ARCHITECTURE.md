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

- OCR reuse requires exact masked, scaled pixels and the same region, scale, and mask.
  Each of two generations retains at most 16 MiB of pixels. Uncached reads still run OCR.
- Windows reuses the current crop's DXGI staging texture. A failed DXGI attempt backs off
  for one second on that monitor while BitBlt continues capturing fresh pixels.
- Two Linux capture backends exist: wlr-screencopy v3 is primary, and portal ScreenCast
  with PipeWire is the fallback.
- Capture backend selection reads the advertised capability. It never reads the compositor
  identity. The `hyprctl cursorpos` cursor rung remains the only cursor-selection exception.
- Screenshot target selection is separate from capture backend selection. On Linux, it can
  query visible-window metadata with Hyprland `hyprctl` or Sway `swaymsg`. These queries
  supply screenshot targets only. They never select a capture backend.
- Buffers use shm with CPU cropping only. No backend contains GPU plumbing.
- The app requests portal consent at startup. A denial never causes an exit. The hover
  path shows one actionable error with a retry action.
- The Worker masks chibipop's own on-screen rectangles in core before OCR. It applies
  a flat white fill with a hard edge.
- An add-time read hides the popup instead of masking it. The popup sits below the anchor, on
  the next lines of the sentence, and a mask would drop those words. The screenshot-on-add
  path already hides and restores the popup, and the sentence probe reuses that path. In
  frozen mode the Worker reads the held frame, so no hide is needed.
- On the portal rung, a live sentence hide records the parked-frame content counter
  before its commit. After the `wl_display.sync` callback, the daemon waits on a short
  calloop poll for a newer counter, or for a 150 ms deadline, and then it sends the
  probe. The `RegionCapture` seam itself never waits for damage.
- The mask boundary is a capture edge. The engine drops words that touch it. It never
  partially recognizes them.
- A wrap joins the nearest body line on the cross axis. For horizontal text, that
  line is below the hovered line. For vertical text, it is the next column to the left.
  The center gap from the hovered line to that candidate is from 0.5 through 4.5
  times the larger line thickness. The candidate line thickness is from two thirds
  through one and a half times the hovered line thickness. The candidate start can
  be up to half a thickness after the hovered line start. The rules use geometry,
  not ruby or heading labels. The reach is fixed. A pitch measured from neighbors
  fails on a two-line paragraph because the pair under test has no pitch beside it.
- The capture box provides the orientation prior. `prefer_vertical` shapes the box, and
  the box shape decides the reading axis of pass 1. A line overrides the box only with at
  least three words whose centers spread along the other axis and whose union is at
  least twice as long as it is thick on that axis. An engine can box the components of
  one large glyph as separate words (issue #92: `新` as `立` over `木`). Two stacked
  words form a column under a spread-only rule. The wrap probe starts at the output
  top edge, and the forward tile runs below the text.
- The capture box grows at most twice around the cursor when it is too small for the
  text under the cursor: a recognized line spans the short side, or the hit word
  touches one edge with at least half the box's thickness, or the engine returns no hit
  and ink under the cursor spans 60 % of the configured short side (`text::ink`), or a
  grown box returns no text. Both sides double because the Linux engine scales a crop to
  detector size from both: a 500 px wide crop is always scaled up 1.92 times, and a
  1000 px wide one 0.96 times. A grown box moves inside the output and shrinks to it
  only when it is larger. A cut read is not an answer. The answer is the last read
  whose hit is not cut. Every grabbed box appears in the outline as a pass-1 box.
- A word box thinner than one sixteenth of its thickness on the reading axis is a
  sliver, not a glyph. The capture seam drops it. The Linux engine returned a `」` in
  a 4 x 92 box at the right edge of a 100 px `規`.
- The build does not include the Windows hide-and-reshow capture guard on Linux.

## Input ladders

- `chibipop-linux` has two channel ladders. Both synthesize the same core `Event`
  objects that the Windows hooks synthesize.
- Cursor rungs: the ext-image-copy-capture cursor session, then portal
  `cursor_mode=METADATA`, then `hyprctl cursorpos` polling. A missing rung means the
  platform is unsupported. A startup diagnostic names the missing capability.
- Trigger rungs: the GlobalShortcuts portal, then a native compositor keybind into the
  control socket.
- The portal shortcut identifiers are `trigger`, `anki-add`, `search`, `sentence-search`,
  `selected-text`, `ocr-clipboard`, and `static-region`. Register only enabled actions with configured chords.
- Hyprland uses native compositor bindings because its portal does not assign keys.
- Apply replaces changed portal registrations without a daemon restart. Retired sessions cannot fire actions.
- Each settings row reports its confirmed portal binding or copies a native bind with `chibipop ctl`.
- The system rejects evdev completely, even as a setting.
- `keyboard_interactivity: none` is a strict rule. The popup never takes focus.
- The control-socket verb set has one verb for each global action. It has `lookup` for Press
  mode. A verb exists only if a user can bind a key to it. No verb reads state, takes an
  argument, or composes.
- A portal press event and its native-bind verb use one code path.
  `shortcuts::action` maps the identifier and trigger mode to a `Verb`. In Toggle mode,
  it maps the trigger identifier to the `toggle` verb. In Press mode, it maps an activated
  trigger identifier to the `lookup` verb and a release to `Action::Nothing`.
  `App::apply_verb` is the only target function.

## Popup and measurement

- Selected application text enters through an explicit capture request and a request-tagged completion event.
- Bins read selections. The Controller rejects stale completions and uses the existing dictionary-only Worker path.
- A selected-text root stays open independently of OCR trigger state. It retains selection text for the sentence field without OCR probing.
- Windows uses UI Automation on an MTA thread without clipboard reads or writes. Unsupported controls return no text.
- Windows selection results include visible text bounds. Selected-text popup height is capped to fit above or below those bounds.
- Selected-text popups use outside-click dismissal in every trigger mode. The Windows observer includes middle and extra buttons.
- `actions.search.selected_opens_sentence_search` routes validated selected text to Sentence search instead of dictionary-only popup lookup.
- Linux PRIMARY supplies no word geometry. Its cursor-based placement cannot guarantee avoidance of the selected word.
- Linux reads PRIMARY through data control on explicit request. Clipboard selection offers are never read.
- PRIMARY lifetime belongs to its source application. An unchanged offer can be read again; a changed in-flight offer invalidates completion.
- Native selection reads are bounded to two seconds and 65,536 UTF-8 bytes. Readers do not cache decoded selections.
- Diagnostics omit selection text. The normal opt-in lookup log can record the resolved headword.

- Hovering painted Japanese text opens a child popup through dictionary lookup, without capture or OCR.
- `popup.sub_popups` enables this behavior. Query assembly skips the invisible ruby word joiner and preserves visible word boundaries.
- Parent popups remain visible. The Controller retains each parent's scroll, selections, and click history.
- Returning to a parent retires its descendants. Root dismissal hides the complete popup chain.
- A hover lookup ignores stale results after navigation. Selection drags do not open child popups.

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
- A click catcher exists in Press mode and for selected-text popups while a popup is placed. The daemon gives every
  output a transparent full-output layer surface at the popup layer. Its input region covers
  the output except the popup rectangle. The catcher is never unmapped. Hide clears its input
  region, and a button press on the catcher hides the popup. The catcher swallows that click
  because there is no evdev path and the popup takes no focus.
- The daemon routes each event of one `wl_pointer` frame by surface. The catcher and the
  popup are one client, so a compositor can put a leave from the catcher and an enter onto
  the popup in one frame. A per-frame owner would hide that enter from the popup, and the
  popup would then ignore every click inside it.

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
- At key press in hold-key mode, the system freezes one full grab of the output under the cursor
  before any popup exists. No capture and no mask run while the user holds the key.
- Release drops the Frozen grab, and each hold-key press captures again. The `toggle` command
  latches the trigger and reads live grabs with the popup masked until toggle-off.
  Toggle mode uses this path on both platforms: Linux sends the `toggle` verb, and Windows flips
  the hook latch.
- Press mode has no Frozen grab or Dwell re-check. Each trigger press performs one live masked
  grab. Text found keeps the popup shown. A press with no text hides it. A press over the popup
  also hides it because the mask gives no text. Screen cursor movement and key release do nothing, and
  per-character lookup is inert.
  Hovering text inside an existing popup can still open a child popup.
- A wrap probe follows pass 1 when the lookup would run past the line end and the box did
  not clip the line. Pass 1 must show no continuation. A continuation near pass 1's lead
  edge can be clipped and does not suppress a probe. The probe uses one bounded
  region when the reading-axis span is at most `2 * TILE_LEN` (1000 physical pixels).
  Otherwise it uses the deterministic sequence of half-overlapping regions needed to
  cover the span from the output margin to the hovered line end. Each reading-axis
  length is at most `2 * TILE_LEN`, and each step is `TILE_LEN` (500 physical pixels);
  the last region can be shorter. For a positive span, the no-failure capture count is
  one when `span <= 2 * TILE_LEN`; otherwise it is
  `1 + ceil((span - 2 * TILE_LEN) / TILE_LEN)`. Each region costs one grab and OCR pass,
  independent of `max_ocr_passes`. A probe stops at its first capture failure and keeps
  pass 1. The probe drops words within `EDGE_MARGIN` of both interior edges before the
  merge; half-tile overlap leaves a full copy of a wide word away from both seams.
  The tile path applies the same rule. `resolve_wrap` merges probe fragments and replaces
  pass 1 only when a continuation joins. When forward tiles add text past the box, the
  stitched head and tail win, and the merge drops the joined wrap. When tiles add no text,
  pass 1's answer with its geometry and any joined wrap stands.
- A sentence probe runs on an Anki add and never on hover. The Controller answers
  `AddRequested` with `Command::RequestSentence`. The bin hides the popup and sends
  `TriggerKind::Sentence` to the Worker. The Worker reads the tiles that
  `text::sentence::probe_regions` names, trims interior tile edges, and cuts the sentence
  with `text::sentence::sentence_at`. The result returns as `LookupOutcome::Sentence`. The
  Controller then builds the note. A failed probe or a probe with no anchor word keeps the
  hover-time sentence. `drain` never drops a `Sentence` trigger, because the user pressed a
  key. The reach is `SENTENCE_REACH_LINES` (6) above and below. Every number is a constant.
- Every cadence number is a hardcoded constant. No cadence number is a setting.

## OCR engine

- `ocr.discard_furigana` filters recognized lines at the shared `TextSource` seam.
  Lookup, sentence probes, and both clipboard paths therefore use one rule.
- meikiocr over `ort` is the only Linux `OcrEngine`. There is no runtime selection, no
  fallback, and no Python runtime.
- The Linux adapter never upscales crops. Windows uses an `UPSCALE` value of 2, and Linux
  uses 1. Measure again before you add upscaling to Linux.
- CI quality floor: horizontal CER <= 5 %, horizontal hit-scan >= 90 %, vertical CER
  <= 20 %, vertical hit-scan >= 75 %. It requires parity with the Python reference
  within 3 percentage points.
- CI box-fit floor: a hit's box must also outline its glyph. Horizontal box fit >= 90 %,
  vertical box fit >= 75 %, and the three large `smoke_2x` glyphs must fit completely. A
  fragment box that contains the glyph center passes hit-scan but fails fit (issue #92).
- CI large-text floor: the screens under `tests/fixtures/large-text/` use `TextSource`
  with the real engine. `新規` at 100, 130, and 160 px and `日本語を話す` at 130 px
  must return the full text from the hovered glyph to the line end. A white-on-black
  news line in BIZ UDPGothic and Noto Sans CJK must return at least to the box edge at
  100 px and at 125 px bold. The tests hover near the top and bottom of a 110 px glyph.
  Every scan rect stays on the hovered line, and the anchor fits the glyph.
  `scripts/render-large-text.py` renders the screens.
- The repository commits models under `crates/chibipop-linux/models/meiki/`. It pins
  their hashes against `SHA256SUMS.txt`. Two steps verify them: `scripts/package-linux.sh`
  when it stages the tarball, and `models::verify` when the engine starts.
- No first-run download path can exist. The app stays offline-first.
- The build statically links ONNX Runtime on linux-x64. It ships no shared object and
  sets no rpath.
- The source-AUR path keeps `--features system-onnxruntime` active. This feature opens
  the distribution library with dlopen.
- Windows discovers providers without changing the saved enablement list. A provider
  starts only when the selected engine is enabled.
- The Windows Worker owns the OCR engine. Apply queues engine and enablement changes through
  the existing Worker reload command, replaces the engine on that thread, and drops the old
  plugin host before publishing active status.
- A disabled selection and a failed plugin start use Windows OCR. A strike-disabled plugin
  shuts down its host before reporting unavailable, which closes its process tree.
- The MeikiOCR adapter caps ONNX and OpenCV worker threads through `config.toml`.
- `scripts/measure_ocr_resources.ps1` reports parent, descendants, and process-tree totals
  for working set, private bytes, CPU, threads, and handles. The 100 MiB and 200 MiB goals
  remain measured targets, not guarantees.

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

- Windows embeds `crates/chibipop-windows/assets/settings-layout.toml`. The typed layout
  owns tab, section, and entry order, labels, and help. Rust owns control identifiers,
  conditions, validation, persistence, and dynamic components. Parsing must reject missing
  or duplicate entries before controls are created. Editing the asset requires a rebuild.
- Windows settings reflow controls in current client dimensions and preserve user sizing.
  Runtime status comes from the concrete OCR backend. Save sequences reject stale results.
  The settings X requests process exit after active writes. The Debug viewer closes independently.
- Interactive Windows commands tee output into a bounded live log. Machine-readable commands
  keep their output contract. Restore original streams before spawning a replacement daemon.
- Settings reject conflicting platform shortcuts before applying or saving changes.
  Users can swap keys in one Apply.
  Windows suppresses lower-priority legacy conflicts in memory and reports them without rewriting the file.
  Priority is Back/Escape, lookup, Anki add, static region, then OCR clipboard.
  Linux validates configured chords. Apply requests direct portal bindings where supported.
  Native compositor bindings remain external configuration.
- Linux settings run as a separate `chibipop settings` process with iced. The daemon
  contains no GUI toolkit.
- The shared `Config` and `SettingsForm` model lives in core. Both platform bins render
  widgets only.
- Live-apply saves the configuration and sends `reload` over the control socket. Windows
  also queues OCR engine changes to the existing Worker command path.
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

- Direct search uses the existing lookup engine and presentation rules without capture or OCR.
- Search displays candidates before a selected definition. Definitions reuse `PopupScene` and the existing native painters.
- Sentence search derives UTF-8 word ranges from enabled dictionary matches through the normal lookup engine, including Japanese inflections and Chinese text.
- Clicks request candidates for the selected word and its shorter internal matches, within the normal result cap.
- OCR-to-clipboard optionally opens Sentence search with the captured text. Linux transfers that text through bounded child stdin, not process arguments.
- Search windows accept native text input. Windows uses native controls; Linux runs a separate iced process.
- Search controls and definitions use the existing CSS parser and theme roles. Candidate summaries use the collapsed text role.
- Windows search reads CSS beside the executable. Linux search reads it beside the active config file.
- Escape dismisses active search windows and invalidates pending replies. Native IME composition handles its own Escape first.
- Windows capture operations observe Escape independently of selector focus. Cancelled OCR waits discard late results without clipboard changes.
- The optional search shortcuts belong to shared `Config` and participate in platform shortcut validation.
- The tray Search item and configured shortcut use the same platform entry point.
- Linux Search holds a runtime focus lock while its input window has focus. The daemon suppresses lookup and actions during that interval.
- Linux replaces portal shortcuts on reload. Only confirmed IDs from the current session can dispatch actions.

- Windows Debug can clear lookup-derived in-memory state while the daemon runs. The ordered
  Worker request validates a freshly opened Dictionary before dropping frozen capture, both OCR
  generations, parsed definitions, stylesheet and frequency caches, and SQLite handles.
- Successful Windows invalidation replaces decoded media caches and dismisses the current popup.
  Media reopen failure leaves media disabled and uses alt text, without retaining stale bytes.
- The Windows Search window clears displayed results and definitions. Separate future queries use
  a fresh SearchService. Linux maintenance transport and separate Search processes remain outside
  this action's scope.

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
- `Hit::process` is outermost first. The step nearest the hovered text comes first.
  `present::inflection_chain` reverses it into build order from the headword outward.
  It drops empty steps and inner parenthesized stems. It keeps the outermost step, so a
  bare te-form still explains its match.
- The Card shows the Inflection chain in one dimmed row after the pitch rows and before
  the part-of-speech row. The separator is ` « ` (U+00AB). An empty chain draws no row,
  so no geometry golden changes.
- The design rejects an outermost-first display (weikipop's order), a relabel table in
  code (the labels come from `data/deconjugator.json`), and a setting that hides the row.

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
