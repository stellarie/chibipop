# Architecture

The orientation map for an agent in this tree: structure, control flow, and the rules
that govern each area. `CONTEXT.md` defines the domain terms. `AGENTS.md` gives the
commands and the workflow. Decisions live in the upstream pull request that made them.

## Project structure

```
src/                    Core lib `chibipop`: all behavior, no OS calls.
  controller.rs         Hover/popup state machine. Event in, Command out.
  worker.rs             Pipeline: capture, mask, OCR, lookup, present.
  text/                 mask.rs, layout.rs, frozen.rs, source.rs.
  lookup/               Deconjugation, SQLite queries, scoring, model.
  dict/                 Import, build, reindex, gloss, media, pitch.
  ui/layout/            Measured layout pass. Builds a PopupScene.
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
packaging/aur/ extras/  PKGBUILDs; desktop file, systemd unit, snippet.
tools/ scripts/         Benchmarks and censuses; package-linux.sh.
```

## Workspace and seams

Three crates. Core owns the behavior and a bin owns the OS. Both bins carry
`[[bin]] name = "chibipop"`, so one link-producing command must never span both.

```
platform bin --Event--> Controller --Command--> platform bin
                            | Trigger
                            v
Worker: capture -> mask -> OCR -> lookup -> present --result--> Controller
```

- `Controller` and `Worker` live in core. A bin runs its native loop, synthesizes
  `Event`s, and executes `Command`s.
- Exactly three seam traits: `RegionCapture`, `OcrEngine`, `TextMeasure`. A trait earns
  its place only where the implementation varies inside one binary.
- Input, tray, popup surface, paths, instance lock, and console handling are
  `Event`/`Command` variants or bin-local functions, never traits.
- All synchronous: std threads and `mpsc`, calloop as the Linux pump, D-Bus and portal
  clients on their own threads. No async runtime.
- Core speaks physical pixels only. The bin converts logical coordinates and fractional
  scale at the seam.
- Painting stays per-platform. Core ships the view model, the theme, and the layout math.

## Capture and masking

- Two Linux capture backends: wlr-screencopy v3 primary, portal ScreenCast plus PipeWire
  fallback.
- Selection reads advertised capability, never compositor identity. The
  `hyprctl cursorpos` cursor rung is the single sanctioned exception.
- Buffers are shm with CPU cropping only. No GPU plumbing inside a backend.
- Portal consent is requested eagerly at startup. A denial never exits: hover shows one
  actionable error with retry.
- The Worker masks chibipop's own on-screen rects in core before OCR. Flat white fill,
  hard edge.
- The mask boundary is a capture edge. Words touching it are dropped, never
  half-recognized.
- The Windows hide-and-reshow capture guard is not built on Linux.

## Input ladders

- Two per-family channel ladders in `chibipop-linux`. Both synthesize the same core
  `Event`s the Windows hooks do.
- Cursor rungs: ext-image-copy-capture cursor session, portal `cursor_mode=METADATA`,
  then `hyprctl cursorpos` polling. No rung means unsupported, with a startup diagnostic
  naming the missing capability.
- Trigger rungs: the GlobalShortcuts portal, then a native compositor keybind into the
  control socket.
- The portal shortcut id set is frozen at exactly two: `trigger` and `anki-add`.
- evdev is rejected outright, even as a setting.
- `keyboard_interactivity: none` is inviolable. The popup never takes focus.
- One control-socket verb per global action. A verb exists only if a user can bind a key
  to it. No verb reads state, takes an argument, or composes.
- A portal press and its native-bind verb resolve through one code path:
  `shortcuts::action` maps the id to a `Verb`, and `App::apply_verb` is the only landing
  site.

## Popup and measurement

- `TextMeasure` takes an ordered list of styled spans plus a wrap width. It returns
  per-line and per-span geometry plus a baseline for each line.
- `TextMeasure` is measure-only, permanently. No painting moves behind it.
- The inline layout pass stays a private module inside `src/ui/layout/`. One
  implementation exists, so it never becomes a trait.
- Both bins convert to any measurer-contract change in the same change.
- `PopupScene` is the measured popup in one uniform, unit-agnostic pixel space. Core
  never sees a scale factor.
- The bin measures and places. The Controller learns the result as
  `Event::PopupPlaced { rect }`.
- The Wayland surface protocol rules live as comments in
  `crates/chibipop-linux/src/popup/surface.rs` and `popup/text.rs`.

## Hover cadence

- Live mode is event-paced with one-in-flight, latest-wins backpressure. No timer drives
  dispatch.
- No settle delay and no velocity gate.
- The damage-gated dwell re-check runs only while a popup is shown.
- Trigger mode freezes one full grab of the output under the cursor at press, before any
  popup exists. No capture and no mask run while the key is held.
- Release drops the frozen buffer, and each press grabs fresh. `toggle` grabs at
  toggle-on and holds until toggle-off.
- Every cadence number is a hardcoded constant. None is a setting.

## OCR engine

- meikiocr over `ort` is the sole Linux `OcrEngine`. No runtime selection, no fallback,
  no Python at runtime.
- The Linux adapter never upscales crops. Windows uses an `UPSCALE` of 2, Linux uses 1.
  Re-measure before ever adding one back.
- CI quality floor: horizontal CER <= 5 %, horizontal hit-scan >= 90 %, vertical CER
  <= 20 %, vertical hit-scan >= 75 %, and parity with the Python reference within 3 pp.
- Models are committed under `crates/chibipop-linux/models/meiki/`, hash-pinned against
  `SHA256SUMS.txt`, and verified twice: by `scripts/package-linux.sh` when it stages the
  tarball, and by `models::verify` when the engine opens.
- No first-run download path may exist. The app stays offline-first.
- ONNX Runtime is statically linked on linux-x64. No shared object ships and no rpath is
  set.
- The source-AUR path keeps `--features system-onnxruntime` live, which dlopens the
  distro library.

## Settings and config

- Linux settings run as a separate `chibipop settings` process on iced. The daemon holds
  no toolkit.
- The shared `Config` and `SettingsForm` model lives in core. Both bins render widgetry
  only.
- Live-apply is save-then-`reload` over the control socket, never a structured push.
- Any setting that must round-trip is a field on the shared `Config`. Never a
  platform-interpreted field, and never a `[linux]` side table.
- No sentinel-value semantics. An unresolvable `popup.font` literal falls back to the
  platform default with a visible warning.
- Each bin renders only its own platform's fields, and a save preserves the other
  platform's fields untouched.
- Autostart state is derived by reading `~/.config/autostart/chibipop.desktop`. No TOML
  field stores it.
- A dictionary rebuild builds beside the old database and atomically renames over it.

## Dictionary and lookup

- A Dictionary's role set is derived by inspecting its banks. Never from a filename, and
  never declared by a user.
- Each role owns its own ordered, independently enabled list. Enabled state is per role
  per Dictionary.
- New imports land at the bottom of each relevant list, enabled.
- Dictionary identity is the exact installed name.
- `term.freq` is a denormalized column, read with no join on the hot lookup path.
- Reindex is an in-place SQL transaction that must never read an archive. It runs on
  every strategy, order, and enable change.
- A Dictionary that does not report a word is not a data point. `DEFAULT_FREQ` is never
  substituted for that silence.
- The displayed frequency is always a real Dictionary's own reported number, from the
  highest-ranked enabled Dictionary, never the computed rank.

## Verification

- Layout changes are verified by committed geometry-snapshot goldens under exact
  equality. No tolerance is permitted.
- Goldens change only through a human-reviewed `workflow_dispatch` bless run. CI holds no
  push credentials and never auto-commits.
- Both test layers are permanent. Neither is deletable scaffolding.
- `fixtures()` and its documentation grow together, enforced by a name-pinning test.
- The CI image is pinned. An image bump is a scheduled re-baseline.

**This coverage is asymmetric.** The golden layer pins real DirectWrite output on Windows
only. `src/ui/layout/tests.rs` runs against `FakeMeasure` and never calls cosmic-text.
Therefore no committed golden pins real cosmic-text geometry.

The methodology lives as comments in
`crates/chibipop-windows/tests/geometry_goldens.rs` and
`crates/chibipop-windows/src/ui/render/geometry.rs`.

## Platform integration

|Windows idiom|Linux equivalent|
|---|---|
|Notify-icon tray|ksni StatusNotifierItem over D-Bus|
|No autostart|`~/.config/autostart/chibipop.desktop`|
|Named per-session mutex|`flock` keyed per display in `$XDG_RUNTIME_DIR/chibipop/`|
|Beside-exe paths|Strict XDG mapping plus portable mode|
|`CREATE_NO_WINDOW`, show/hide console|No analogue: stderr plus a truncated logfile|
|Opt-in lookup log|The same gate, `debug.show_lookup_log`|

- Tray failure is never fatal on Linux. The app is fully operable trayless.
- The instance lock and the control socket must be keyed identically.
- Library archives and the built database live under `$XDG_DATA_HOME`, not under cache,
  so a cache cleaner cannot purge them.
- Portable mode versus XDG mode is decidable from one file's existence. The app never
  dual-reads.
- Lookup content reaches disk only when `debug.show_lookup_log` is on. Diagnostics always
  reach stderr and the logfile.

## Packaging and CI

- A release ships one tarball, `chibipop-vX.Y.Z-linux-x64.tar.gz`, plus two AUR packages:
  `chibipop-bin` repacks the tarball, `chibipop` builds from the release tag.
- The asset naming shape is a forever contract: every shipped binary parses
  `releases/latest` asset names, so the `chibipop-v` prefix and the `-linux-x64.tar.gz`
  suffix must never change shape.
- Self-update on Linux is check-only and button-only. It never writes and never phones
  home at startup.
- The `.new` and `.old` exe swap stays Windows-only.
- CI runs two native jobs with mirrored gates and no cross-compilation. Each job excludes
  the foreign bin crate.
- The CI runner image is pinned to the oldest image the statically linked `ort` prebuilt
  can link on, today `ubuntu-24.04`.

**The runner pin and the `ort` linkage are coupled, so re-verify the runner floor on
every `ort` bump.** A 2026-08-26 discovery that the prebuilt is statically linked on
linux-x64 raised the floor from `ubuntu-22.04` to `ubuntu-24.04`. 22.04 still schedules,
but it cannot link `__isoc23_strtoull` (glibc 2.38) or `_M_replace_cold` (GCC 13
libstdc++).
