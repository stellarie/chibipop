# The measurement seam takes styled spans

Amends ADR-0004's "**`TextMeasure` is measure-only**: wrap a string at a max width, return
line metrics. Nothing else." It stays measure-only. It stops being one string. Research
basis: `docs/research/dict-shapes.md`, the census of 97 real archives that sizes the
dictionary-rendering-parity work (upstream #31).

## What changes

**The seam takes an ordered list of styled spans plus a wrap width, and returns per-line
and per-span geometry plus a baseline per line.** A **styled span** (`CONTEXT.md`) is a run
of text with one resolved style — family, size, weight, italic, colour — and it is the
finest unit the seam addresses. The term is deliberate: "text run" is already the scene's
positioned-run vocabulary, and the two are not the same thing.

Today `MeasureRun` is one `&str` carrying one `font`, `size`, `weight`, `italic`, and
`max_w`, and `Metrics` is `{ w, h, lines }` — one aggregate box for the whole string
(`src/ui/layout.rs`). A run boundary is therefore always a line boundary, so bold text and
normal text cannot share a wrapped line. That is the single fact this amendment exists to
change.

**Measure-only survives intact.** No painting moves behind the trait, `PopupScene` still
carries positioned geometry as plain values rather than platform handles, and the core
layout tests still drive `layout::scene` with a fake measurer returning fixed metrics
(`src/ui/layout/tests.rs`, ADR-0011's first test layer). ADR-0001's "exactly three traits"
is untouched: no trait is added, and `TextMeasure` is still the measurement seam.

**`caret_boxes` widens to the same span model and keeps its current behaviour**, because
the per-character headword drill-down zips its boxes 1:1 with the kanji of a headword, and
the inverse mapping — screen point back to a span offset — is what a later
sense-selection feature needs.

**Baseline is new and is required.** With one style per run a line's box was enough: the
Linux adapter derives height as `lines × size × LINE_HEIGHT`, a whole number of uniform
advances, and the scene stacks those. Mixed styling on one line ends that arithmetic —
line height is no longer uniform, and nothing in `{ w, h, lines }` says where the glyphs
of a smaller span should sit inside the taller line. The census makes this concrete: 18
dictionaries set `verticalAlign` over 632 811 nodes and 18 set `fontSize` over 772 971
nodes, and 30 of the 52 structured-content dictionaries emit `img` over 386 141 nodes, 20
of them declaring a `height` and 21 a `sizeUnits` — `1em` in the middle of a word. A
superscript reference mark, a subscript, and a gaiji image at text size are all positions
*relative to a baseline*. Without one there is no arithmetic to place them, only a guess.

## Why this is a real requirement and not a speculative one

**Two real adapters, exercised on real content.** ADR-0004 widened ADR-0001 only when a
second measurer arrived, on the rule that a seam earns its keep by varying *within* a
binary rather than in principle. The same test passes here: DirectWrite
(`crates/chibipop-windows/src/ui/render.rs`) and cosmic-text
(`crates/chibipop-linux/src/popup/text.rs`) both implement `TextMeasure` today, and both
will implement the wider contract against the same dictionaries. This is not a seam built
for a hypothetical second implementation; it is an existing seam whose contract is too
narrow for content 30-plus dictionaries in one real library already ship.

**The narrow seam cannot be worked around in core.** Measuring each styled fragment
separately and summing advances does not wrap a mixed-style paragraph correctly: the break
opportunity is a property of the whole line as the shaper sees it, and font fallback and
inter-glyph spacing do not stop at a style boundary. Core has no shaper and must not grow
one. So the paragraph goes to the measurer whole, or it is laid out wrong.

## Neither platform gains capability

**DirectWrite already takes per-range formatting on one layout.**
`IDWriteTextLayout::SetFontWeight`, `SetFontStyle`, `SetFontSize`, `SetFontFamilyName`, and
`SetDrawingEffect` each take a `DWRITE_TEXT_RANGE` and apply after the layout exists;
per-line height and baseline come from `GetLineMetrics`
(`DWRITE_LINE_METRICS::baseline`), and per-span extents from `HitTestTextRange`. The
adapter today creates the layout from one cached `IDWriteTextFormat` and asks only
`GetMetrics` and `HitTestTextPosition` — it has never asked for the rest.

**cosmic-text already takes rich text with per-span attributes.**
`Buffer::set_rich_text` takes `(&str, Attrs)` pairs plus a default `Attrs`, and `Attrs`
carries `family`, `weight`, `style`, `color_opt`, and `metrics_opt` — per-span font size
and line height. Per-line baseline is `LayoutRun::line_y` ("Y offset to baseline of line"),
beside `line_top` and `line_height`; per-span extents come from each `LayoutGlyph`'s
`start`/`end` byte range with its `x` and `w`. The adapter today calls `set_text` with one
`Attrs` and reads only `line_w`.

**So only the Rust contract is new.** Both adapters keep one shaping path shared between
measure and paint — `Text::layout` on Windows, `TextEngine::shape` on Linux — so a run is
never wrapped one way and painted another. Colour rides the span for that shared path's
benefit; it is not a measurement input, and no geometry depends on it.

## What is rejected

- **An opaque shaped-layout handle in the scene** — infects core with generics, already
  rejected by ADR-0004. It also puts a platform object's lifetime inside `PopupScene`,
  which the layout tests produce from a fake measurer with no font and no platform object
  anywhere in the type.
- **Metrics plus a cache key** — an eviction policy in both bins, and stale keys paint
  text that does not match the geometry. Already rejected by ADR-0004, and spans make it
  worse: the key would have to cover the whole ordered span list and its styles, so it
  grows precisely where staleness is hardest to notice.
- **Exposing the inline layout pass as a trait** — there is exactly one implementation of
  it and there will only ever be one, so by ADR-0001's own rule it is a hypothetical seam.
  It stays private to `src/ui/layout.rs` and is tested through `layout::scene` against
  fixed metrics.
- **Keeping the one-string seam and wrapping in core** — the previous section's argument:
  it moves line breaking to the side of the seam that has no shaper.

## Both bins convert in the same change

Per ADR-0004's standing rule, and for its reasons: a Linux-first conversion opens a
duplication window that quietly becomes permanent, and a Windows-first one designs the
interface with only DirectWrite in hand.

The gate is ADR-0011's geometry snapshots. The seam widens with **no visible change**: a
single-span request must return metrics identical to the current seam for the same input on
both platforms, so the snapshot diff for that step is empty. An empty diff is the only
available proof that the adapters were not altered while the contract moved, which is why
the inline formatting pass is a separate change and not part of this one.

## Consequence for ADR-0011

ADR-0011 still holds — two permanent test layers, exact equality with no tolerance, bless
by `workflow_dispatch` and human commit. Three things follow from this amendment.

**The snapshot schema changes.** Today it is per-element text, font size, and x/y/w/h, hit
rects with their `HitAction`s, content height, `max_scroll`, and side-panel geometry
(`crates/chibipop-windows/src/ui/render/geometry.rs`). It gains per-span geometry and
style, the baseline, the box record, and image rects with their media keys. Floats stay at
the existing fixed precision.

**New fixtures are authored rather than merely re-captured.** ADR-0011's capture property
— "capturable from the unmodified pre-refactor build" — does not extend to styled
content, because the unmodified build has no styled spans to capture: its fixture helper
takes plain strings (`block(dict, glosses: &[&str])`), and none of the seven fixtures can
express mixed styling. So mixed-span, bordered-pill, nested-list, table, ruby, and
image-bearing fixtures are written by hand and blessed from the new build, with the
existing seven's intent preserved. ADR-0011's fixture-set section and `fixtures()` grow
together.

**Every snapshot is re-blessed.** This is the sanctioned path ADR-0011 already names for an
intentional layout change, not a workaround: bless on the branch, review the geometry diff
in the PR, land the goldens alongside the change. Each moved coordinate is attributed to a
specific intended change; a coordinate nobody can explain blocks the bless. After the
re-bless the snapshots still defend the platform adapters, which nothing else exercises —
now including their per-range formatting and baseline reporting.
