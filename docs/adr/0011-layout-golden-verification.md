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

**Widened by ADR-0013**, which the fixture set below reflects: an element also carries
its styled spans, the line and span geometry the measurer reported for them with the
baseline, its box record, its readings, its list markers, its dictionary address, and
its image's media key. Same fixed precision, same exact equality.

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

**Thirteen** hand-built `Presentation`s, each one committed golden. The first seven
are the original set, in the `one_card`/`with_collapsed` style, and their intent is
unchanged:

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

All seven still build their blocks from plain glossary strings, which is what 20 of
the census's 72 dictionaries emit, so none of them changed shape when `GlossBlock`
became one dictionary's contribution with `entries: Vec<GlossEntry>`:
`GlossBlock::parse` produces exactly the one-row block they always described.

The last six are ADR-0013's, and they are **authored rather than captured**, because
the pre-refactor build had no styled spans to capture from. Every tree in them is a
real corpus shape out of `docs/research/dict-shapes.md`, so a golden that moves says
something about a dictionary somebody owns:

8. **Mixed styled spans** — bold, a larger and a smaller size, italic, colour, and a
   `sup`/`sub` pair, all in one wrapped paragraph. The one fact ADR-0013 exists to
   change, and the only fixture that can produce a non-zero `shift` or a span shorter
   than its line.
9. **Bordered pill** — the box record, both mechanisms. A `css` variant with
   Jitendex's own `span[data-sc-class="tag"]` pill, and an `inline` variant carrying
   a bordered block, a marker-carrying spacing-only inline box that resolves to no
   box, and the same bordered block with the marker span *first* — the shape that
   used to lose its box entirely (see the finding below).
10. **Nested list** — hanging markers, two levels. A `jitendex` variant with the real
    `ul[sense-groups]`/`ol`/`ul[glossary]` tree, its two CSS list rules, its example
    pair and its attribution line; a `plain` variant with the default bullet and
    number ladder. The example and attribution lines are the only place a golden sees
    the popup's role default, which now keeps them.
11. **Table with both spans** — a conjugation grid with `colSpan` and `rowSpan`, where
    `Table`, `Cell` and the cells' own paragraphs meet.
12. **Ruby run** — readings positioned over their bases, including a reading that
    overhangs a one-character base and a styled `rt`. Also the only fixture with two
    term-bank rows under one dictionary label.
13. **Image-bearing entry** — the census's three representative `img` nodes: a
    recorded `1em` GIF gaiji, a monochrome SVG declaring both axes, and an unrecorded
    asset that falls to its `alt` text. Where a media key reaches a golden.

`fixtures()` and this list grow together, and
`the_fixture_set_is_the_thirteen_from_adr_0011` pins the names against it.

### One finding this set recorded, and the fix that closed it

In the `bordered_pill` `inline` variant, a block whose **first** child carries a
`data.content` marker used to lose its own box: the marker opened a paragraph, the
block box attached to the first paragraph the block emitted, and there was none. The
same block with one text node before the marker kept its box. Both shapes sit side by
side in that one variant, which is what the fixture was authored for.

**Closed.** A block's box is now a container around *every* paragraph the block emits
(`layout::Boxed`), which is what a browser draws: a bordered `div` holding three
paragraphs draws one border around all three. So the fixture's two shapes now agree —
each `div` draws one box — and the variant's element list grew by one textless
`Block` element per boxed `div`, ahead of the paragraphs it frames. No golden was
blessed: this variant is one of the thirteen still awaiting the `windows-2025`
dispatch, so the capture will record the fixed answer the first time it runs.
