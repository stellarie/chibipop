# Golden verification of the layout extraction

Guards ADR-0004's "both bins convert in the same change" clause. The layout engine welded
to DirectWrite in `src/ui/render.rs` moves into core with **no local Windows dev loop** —
every capture and every red diff is a CI round-trip, which shapes all five choices below.

## Two permanent test layers

1. **FakeMeasure core tests** — the primary, platform-free contract. Core `layout` driven
   by a fake `TextMeasure` returning fixed metrics runs deterministically in both CI jobs
   forever, covering wrapping, gap stacking, scroll culling, side-panel geometry, and hit
   rects with zero font or OS dependency.
2. **DirectWrite geometry-snapshot goldens** — the adapter gate, riding tier0's normal
   `cargo test`. After the transition they defend the DWrite `TextMeasure` adapter itself,
   which nothing else exercises.

Rejected: goldens as deletable transition scaffolding (the adapter would lose its only
regression net) and goldens-only with no FakeMeasure layer (core layout logic would be
tested nowhere but Windows CI).

## The golden is a geometry snapshot

One JSON file per fixture: per-element text, font size, and x/y/w/h; hit rects with their
`HitAction`s; content height; `max_scroll`; side-panel geometry. Floats serialized at
fixed precision.

Capturable from the **unmodified pre-refactor build**: `layout_pass` already computes
every one of these (its `target` is `Option` — the measure-only walk needs no window, so
capture is a plain `cargo test` on the runner). Post-refactor, the `PopupScene` projects
onto the same schema. A red diff names the exact element and coordinate that moved —
debuggable from a Linux box reading CI logs.

Rejected: serializing `PopupScene` itself (the pre-refactor capture would have to
fake-produce a type that doesn't exist yet — a proto-refactor, exactly what "captured
from the unmodified build" forbids); rendered-image hashes (needs a render target, hostage
to ClearType/antialiasing, and a mismatch is undebuggable remotely); digest-only goldens
(every red costs a CI round-trip just to see what moved).

## Exact equality, no tolerance

DirectWrite metrics are deterministic for a fixed font file and identical calls, and the
extraction changes **zero measurement calls** — before/after on the same image must be
bit-identical. A tolerance would mask precisely the bug class this gate exists to catch:
rounding moved from `render.rs` into core, off-by-one gap accounting, scroll-culling
boundary shifts. Drift enters only via runner-image font updates, which is a re-baseline
event, not a tolerance problem.

## Bless mechanics

The golden test itself writes goldens instead of asserting when `CHIBIPOP_BLESS=1`. A
`workflow_dispatch` job runs it and uploads the goldens as an artifact; a human downloads,
reviews, and commits. Capture and verify are the same code path; CI holds no push
credentials (auto-commit rejected), and no separate capture binary exists to drift.

Transition sequence: bless on pre-refactor `main` → review + commit goldens → the
extraction PR must pass verify against them.

**Intentional layout changes** later: run the bless dispatch on the branch, review the
geometry diff in the PR, land new goldens alongside the change.

## tier0 pins its image

`runs-on` moves from `windows-latest` to the explicit current image (`windows-2025`),
with a comment. tier0 already treats its environment as a baseline — the clippy error
count, the 400-test floor — so a floating image was never really part of its contract; a
label migration changing fonts under the goldens would red the gate for no code change.
Image EOL becomes one scheduled re-baseline: bless on the new image, commit stating
re-baseline vs finding, per the existing baseline culture in `ci.yml`. Rejected: a
separate pinned golden job (second Windows job, a test-name filter that rots, and the
goldens still fail inside tier0's own `cargo test` on drift — the problem reintroduced).

## Fixture set

Seven hand-built `Presentation`s in the existing `one_card`/`with_collapsed` style, each
one committed golden:

1. **Wrapping-heavy long gloss** — the wrap loop, `LINE_GAP`/`SECTION_GAP` stacking.
2. **Side panel, both modes** — the same collapsed-row content with `side_panel`
   true/false; `SIDE_PANEL_W`/`SIDE_GAP`, `side_measure` height.
3. **Scrolled content** — taller than view, nonzero scroll; culling, `max_scroll`,
   scrollbar thumb.
4. **Match highlight** — `HIGHLIGHT_PAD` against measured glyph geometry.
5. **Full chrome** — back button, Anki slot, frequency corner; every `HitAction`
   variant's rect.
6. **Minimal/edge cards** — no reading, no PoS, unranked, empty gloss; degenerate gap
   paths.
7. **Kitchen sink** — everything at once, the cheap catch-all.
