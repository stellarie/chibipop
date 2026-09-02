# What a hover costs, and whether it can afford a parse

Measured 2026-08-29 with [tools/hover-parse-bench](../../tools/hover-parse-bench/README.md)
against the same corpus as [dict-shapes.md](dict-shapes.md), on an AMD Ryzen 7
9800X3D under `rustc 1.98.0`, stock `--release`.

This doc exists to settle one assertion made by the dictionary render-parity
effort: "hover runs roughly 25 point queries and cannot afford a parse", on
which that effort parses structured content at dictionary build time and
stores the typed tree.

**Superseded in two places by [lookup-cost.md](lookup-cost.md).** The fan-out is 139
point queries at p50, not 25, and 131 of them miss; and the 63 µs figure below is one
probe, not a hover - a real `LookupEngine::run` costs 1 012 µs at p50, of which
`entries` and its glossary parse is 3.0%. Neither correction changes this doc's
verdict, and the second strengthens it.

**It can afford it.** On a realistic 12-dictionary library, parsing the glossary of
every row a hover renders costs **154 µs at p50 and 1.8 ms at p99**, which is 1.0%
and 11% of a 16 ms frame. The measured maximum over 550 real headwords is 3.6 ms.

## Why the number is small: `MAX_RESULTS`

`LookupEngine::run` sorts candidates and calls `ranked.truncate(MAX_RESULTS)`
**before** it calls `entries` (`src/lookup/engine.rs:175-179`, `MAX_RESULTS = 10`).
No hover deserializes more than ten entries, however many rows the surface matched.
The parse cost is bounded by ten glossaries, not by the 2 173 term-bank rows the
worst headword in the corpus matches. That cap is the fact the spec's assertion
overlooks.

## Cost per hover, realistic 12-dictionary library

"Hover today" is `terms_for` + `entries`, including the `Vec<Sense>`
deserialization `SqliteDictionary::entries` already runs. "Parse added" is
`serde_json` over the raw Yomitan glossary of the ten rendered rows plus a full
recursive tree walk.

| | hover today (µs) | + parse added (µs) | sum (µs) | sum / 16 ms |
|---|---:|---:|---:|---:|
| p50 | 63.3 | 153.8 | 217.1 | 1.4% |
| p95 | 98.1 | 785.6 | 883.7 | 5.5% |
| p99 | 133.1 | 1 811.9 | 1 945.0 | 12.2% |
| max | 213.5 | 3 634.8 | 3 848.3 | 24.1% |

Aggregate parse throughput: **180 to 210 MB/s**, single-threaded. Two full runs
agree within 2% on every percentile.

The worst-case headwords - the 50 with the most glossary bytes unioned with the 50
with the most rows, all short kana surfaces such as `こう` and `し` - are *cheaper*
to parse (max 285 µs) than the frequency sample, because `MAX_RESULTS` bounds them
to ten glossaries. They are expensive in `terms_for`, which returns 810 rows for
`こう`, not in the parse.

Ceiling if the ten-result cap were ever lifted: 4.1 ms max on the library,
6.0 ms against all 97 archives, and 13.9 ms for a worst-case kana surface on the
library. Only `こう` against all 97 archives with the cap removed breaks the frame,
at 57 ms - a hypothetical on top of a hypothetical.

## What a hover already deserializes

| 12-dictionary library | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|
| entries matched | 14 | 40 | 47 | 67 |
| `senses` JSON bytes, ten rendered rows | 14 932 | 58 901 | 123 792 | 246 144 |

chibipop already parses 14.9 KB of JSON per hover at p50 and the whole query path
still costs 63 µs. The counterfactual parses roughly twice those bytes, because raw
structured content carries the markup the flattened `senses` column threw away.

## Payload volume, and what importing everything costs

| glossary JSON bytes per hover | realistic library (12) | all archives (97) |
|---|---:|---:|
| p50 | 58 890 | 97 069 |
| p95 | 306 156 | 728 101 |
| p99 | 570 825 | 1 873 492 |
| max | 2 475 509 | 10 926 642 |

Importing every archive multiplies the median hover by 1.7x and the p99 by 3.3x.
The tail grows far faster than the middle.

## The baseline not to use

The live database on the development machine holds **one** dictionary
(Jitendex): 435 448 entries, `schema_version` 2. A hover against it costs 7.2 µs at
p50 and touches one entry. That is the cheapest possible case and understates a
multi-dictionary hover by about 9x. The numbers above therefore come from a
throwaway 12-dictionary database built with chibipop's own `dict::build::build`
(2.6 M entries, 3 007 MB, 64 s to build), deleted after the run.

## Consequence: store the raw JSON, parse per hover

Three storable forms, and the trade between them:

| stored form | hover parse | a parser fix costs |
|---|---|---|
| raw structured-content JSON | measured: +154 µs p50, +1.8 ms p99 | nothing - reparse on next hover |
| typed tree as JSON (spec's choice) | a JSON parse of comparable size, so roughly the same | a full rebuild of every dictionary |
| typed tree in a binary encoding | a decode, materially cheaper than either | a full rebuild of every dictionary |

The middle row is the one to drop. Storing the typed tree as JSON in the `senses`
column does not remove the hover parse - it swaps parsing raw structured content
for parsing a serialized `GlossDoc` of comparable size - so it buys nearly nothing
against the first row while costing a rebuild on every future parser fix.
[INFERENCE] The "comparable size" claim is reasoned, not measured: `GlossDoc` does
not exist yet, and a lossless tree serializes to roughly its input plus variant
tags.

So: keep the raw glossary JSON in the stored record and parse it per hover, behind
a small parsed-tree cache keyed by entry id. Revisit only if the p99 ever matters,
and then by moving to the third row rather than back to the second.

## What this does not measure

- **`GlossDoc` does not exist yet**, so `parse` times `serde_json::Value` plus a
  walk. That is an over-estimate of the typed cost, but by less than first claimed
  here: prototypes measure a typed parse of this shape at **1.16x** cheaper than
  `Value`, not 1.5-3x, because the JSON tokenizer is shared. The 1.5-3x is real in the
  **walk** (6.5x) and in memory (4.6-6.0x). See [lookup-cost.md](lookup-cost.md).
- **The hover baseline is a floor** - and [lookup-cost.md](lookup-cost.md) has since
  measured its height. It times one `terms_for` plus one `entries`,
  not the full deconjugation fan-out. The parse column is unaffected, since
  `entries` is called once per hover over the union of candidate ids, but the ratio
  between the two columns flatters the query side. The verdict is stated against
  the 16 ms budget rather than against that ratio.
- **Warm cache, one thread.** Each headword is swept five times and the median
  sweep kept. A cold first hover pays page faults no repeat sweep sees.
