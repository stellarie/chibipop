# Workspace layout and platform seams

For Linux/Wayland support the repo becomes a three-crate workspace: core lib `chibipop`
(keeps its name so `use chibipop::…` survives) plus bin crates `chibipop-windows` and
`chibipop-linux`, each with `[[bin]] name = "chibipop"`. Platform code lives as modules
inside each bin — a bin *is* the platform adapter; no separate platform-lib crates until
something else needs to link one.

**Core owns the behavior; bins own the OS.** The hover/popup state machine is extracted
from `app.rs` into a core `Controller` (`Event` in, `Command` out), and the capture→OCR→
lookup→present pipeline into a core `Worker` (`Worker::spawn` → trigger/result channels,
preserving today's snapshot-reload semantics). Each bin runs its native loop (GetMessageW
/ calloop), synthesizes `Event`s, and executes `Command`s. Rationale: the live-hover feel
is the product; duplicating its arming/debounce/lifecycle logic per platform guarantees
drift. Risk accepted: this is a behavior-preserving refactor of the Windows path.

**Exactly three traits.** `RegionCapture` — pull-shaped (`grab(region) -> Frame`), with
the invariant "most recent content, never block on damage"; push-shaped backends (portal/
PipeWire) buffer internally so one weird backend absorbs the mismatch. `OcrEngine` —
image in, lines with per-word geometry out. `TextMeasure` — text wrapped at a max width
in, line metrics out; measure-only, painting is never behind it (added by ADR-0004 when a
second measurer — cosmic-text beside DirectWrite — made the layout seam real; originally
this ADR said "exactly two"; **amended by ADR-0013**, which keeps it measure-only and one
of three but takes styled spans rather than one string). Each earns a trait because it
genuinely varies *within* a binary (compositor families; benchmarked OCR candidates;
per-platform text stacks). `TextSource` becomes the core-internal facade composing
capture and OCR with the shared layout/hit-scan logic.

**Everything else is vocabulary or data, not traits.** Input (hooks / hyprctl / portals)
and the popup surface are `Event`/`Command` variants; tray is an event source plus
commands; paths are a plain struct the bin computes; instance lock and console handling
are bin-local functions. One adapter per binary = hypothetical seam; don't build it.

**All sync.** std threads + mpsc everywhere, calloop as the Linux pump, D-Bus/portal
clients on dedicated threads feeding channels. No async runtime — the app is
event-driven, not I/O-concurrent.

**Core speaks physical pixels only.** OCR and hit-testing operate on captured pixels;
logical-coordinate and fractional-scale conversion happens in the platform bin at the
seam.

**Rendering is per-platform.** Core ships the view model (`present.rs`), theme, and
layout math; each bin hand-paints (D2D/DW today; tiny-skia-class on Linux). A shared
draw-command IR was rejected as a hypothetical seam that would force churn on the
regression-forbidden Windows renderer; revisit only if two renderers visibly duplicate
logic. **Amended by ADR-0004**: they did — layout moved into core as a `PopupScene` of
positioned runs behind a `TextMeasure` seam, with both bins converted together under
golden measurement tests. Painting remains per-platform.
