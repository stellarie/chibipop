# Where a hover's time actually goes

Measured 2026-08-29 with [examples/lookup_breakdown.rs](../../examples/lookup_breakdown.rs)
and [examples/gloss_arena_bench.rs](../../examples/gloss_arena_bench.rs), on an
AMD Ryzen 7 9800X3D under `rustc 1.98.0`, stock `--release`, one thread pinned with
`taskset`, against the same corpus and the same two databases as
[hover-parse-cost.md](hover-parse-cost.md).

That doc measured one `terms_for` plus one `entries` and said so ("the hover baseline
is a floor"). This measures the floor's height, and the answer changes where
optimisation effort belongs.

## One hover, split

`LookupEngine::run` over a 25-character OCR line - `MAX_LOOKUP_CHARS`, what
`worker.rs:466` hands `run` when the line under the cursor is long enough.

| component | 12-dict library, p50 µs | % | live (Jitendex only), p50 µs | % |
|---|---:|---:|---:|---:|
| deconjugation (25 prefixes x 104 rules) | 228.5 | 22.6% | 224.1 | 27.1% |
| `terms_for` on **hits** | 80.9 | 8.0% | 30.2 | 3.7% |
| `terms_for` on **misses** | **553.5** | **54.7%** | **537.0** | **65.1%** |
| `entries`, today's `Vec<Sense>` parse included | 30.0 | 3.0% | 5.1 | 0.6% |
| group + rank + sort + build hits | 81.4 | 8.0% | 14.0 | 1.7% |
| **`LookupEngine::run`, total** | **1 011.7** | | **825.5** | |

Anchored to the earlier doc: the same `terms_for` + `entries(top 10)` quantity
reproduces at 59.8 µs against its 63.3 µs on the library, and 7.1 µs against its
7.2 µs on the live database. Same measurement, more of it.

The worst hover measured is a real input, 「ていただく・盗み見る・盗み見する・盗視する・目を通」:
185 point queries, 65 002 term rows, **51.3 ms** - three frames - of which `terms_for`
is 38.6 ms and grouping plus sorting 11.6 ms.

## The fan-out is 139 queries, not 25, and 94% of them miss

| point queries per hover | library p50 | p95 | max |
|---|---:|---:|---:|
| 25-char line, total | **139** | 204 | 291 |
| of which hit | 7 | 23 | 61 |
| of which **miss** | **131** | 196 | 278 |
| 8-char line, of which miss | 34 | 53 | 89 |
| bare headword, of which miss | 4 | 11 | 43 |

The spec's "roughly 25 point queries" is low by 5.6x at p50 and 11x at the maximum.

A miss costs **4.2-5.1 µs**, and it costs the same on a 435 k-entry database as on a
2.6 M-entry one (4.22 vs 4.30 µs). It is not b-tree depth; it is fixed per-call
SQLite overhead - 4.28 µs of `prepare_cached` + bind + step, with 0.8 µs or less of
Rust on top. Misses were generated two independent ways that agree within 10%,
including 37 855 distinct **real** misses recorded from `LookupEngine::run` itself.

## The schema is right, and the statement cache is already earning

```
terms_for:  SEARCH term USING INDEX idx_term_surface (surface=?)
entries:    SEARCH entry USING INTEGER PRIMARY KEY (rowid=?)
```

Verbatim `EXPLAIN QUERY PLAN` on both databases. Neither hot query scans.
`prepare_cached` is 1.72 µs cheaper per call than `prepare`, and a hover makes 139
calls, so the statement cache already saves roughly **240 µs per hover**. It is the
largest optimisation in the code today.

## The biggest available win: a bloom filter over `term.surface`

Built for real over every distinct surface, probed with the 37 855 recorded misses,
false positives counted rather than assumed.

| | library | live |
|---|---:|---:|
| distinct surfaces | 1 514 333 | 572 513 |
| filter size, 10 bits/key, k=7 | **2.1 MB** | 1.0 MB |
| build cost at startup | 0.2 s | <0.1 s |
| probe, swept over 3 000 distinct keys | **0.053 µs** | 0.053 µs |
| `terms_for` on the same keys | 4.98 µs | 4.12 µs |
| false positives | 1.39% | 0.32% |
| net saving per rejected miss | **4.88 µs** | **4.08 µs** |

Applied per hover:

| one 25-char hover | library p50 | live p50 |
|---|---:|---:|
| `LookupEngine::run` today | 1 205.6 | 823.0 |
| with misses rejected by the filter | **519.6** | **293.7** |
| saved | **652.5 µs (54%)** | **525.2 µs (64%)** |

This is what hoshidicts's architecture is really buying with its `bloom.filter` and
`ankerl::unordered_dense` (`src/query.cpp:46-61,120-151`): not faster glossary
decoding, but the elimination of per-probe database calls. Its other moves -
`mmap`'d blob and index files, and `materialize()` deferring zstd decompression until
after ranking (`src/query.cpp:492-496`) - map onto the two findings below.

Caveats, honestly: the saving scales with input length, because misses do. On a bare
headword the filter saves about 20 µs and is pointless. It earns its keep on running
text, which is what OCR hands the engine. The filter must be rebuilt when the library
changes, which costs 0.2 s, so build it at startup and never persist it.

## The second win: an arena for **rows**, not for glosses

Owned `TermRow` mapping, not SQLite, dominates a high-row surface.

| 「こう」, 862 rows | µs | share of `terms_for` |
|---|---:|---:|
| SQLite: index seek + 862 steps, no column decoded | 161.1 | 22% |
| + every column decoded, borrowed (`ValueRef`, zero copy) | 234.3 | 32% |
| + mapped into `Vec<TermRow>` (what `terms_for` returns) | 734.7 | 100% |
| **row-mapping allocation alone** | **500.2** | **68%** |

On the worst-case headword set that mapping is 185 µs p50 (53% of `terms_for`); on
the median frequency headword it is 5.6 µs (28% of `terms_for`, 0.6% of a hover). The
grouping that follows clones `written` and `reading` a second time, worth up to a
further 47 µs p50 on the worst set. **[MODELLED]** ceiling of a borrowed or
arena-backed row read is the measured `mapped − borrowed` delta: 500 µs for 「こう」.

## Where the gloss representation lands: on maintainability, not latency

`entries` is 3.0% of a real hover on the library and 0.6% on the live one, and serde
is a fifth of that: 6.0 µs, deserializing the flattened `senses` column at
**3.2 GB/s**. The heap copy an arena would remove is 0.37 µs, **0.04% of a hover**.

So latency cannot choose `GlossDoc`'s shape. Three representations were prototyped
and held to an asserted-identical walk and content checksum over 5 200 real payloads
(630 442 nodes):

| per hover, p50 | parse µs | walk µs | parse+3 walks µs | allocs | retained B/node | style probe µs |
|---|---:|---:|---:|---:|---:|---:|
| `serde_json::Value` + walk | 94.8 | 9.8 | 123.6 | 3 745 | 678.2 | 11.1 |
| box tree (`Vec<Node>`, `String`, `HashMap`) | 81.5 | 1.6 | 86.1 | 3 085 | 510.9 | 7.8 |
| **flat arena** (spans, interned keys) | **78.8** | **1.5** | **82.9** | **383** | **106.5** | **2.2** |

The arena is faster or equal on every percentile of every axis, but its parse-time win
over the box tree is 2.7 µs - noise. What is not noise:

- **8.1x fewer allocations** per hover, **4.6x less retained heap**, and **12.1x
  faster to free** (20.5 µs -> 1.7 µs). This is the parsed-tree cache the spec now
  wants: a box tree costs 1 817 live allocation blocks and 384 KB per hover-worth,
  an arena 186 blocks and 84 KB, cloneable in six `memcpy`s and free behind an `Arc`.
- **The style probe is 3.6x faster at p50 and 17.1x at p99** (7.8/140.3 µs
  -> 2.2/8.2 µs). 27.8% of all nodes carry one of the top 20 `data-*` keys, so this
  is the CSS matcher's inner loop, and it runs per node per hover.
- Going **typed at all** is the timing win, not the arena: `Value` -> box tree cuts
  the render-shaped p99 by 1.6x, box tree -> arena by a further 1.36x.

One correction to [hover-parse-cost.md](hover-parse-cost.md): its note that the
`Value` figure "over-estimates a typed parse by 1.5x to 3x" is too optimistic on the
parse. A typed parse of this shape is 1.16x cheaper than `Value`; the 1.5-3x shows up
in the **walk** (6.5x) and in memory (4.6-6.0x). The JSON tokenizer is the same in all
three, and that is where the parse time goes.

## Verdict, in effort order

1. **Bloom filter over `term.surface`** - 652 µs of a 1 206 µs hover, 54%. Backlog 37.
2. **Borrowed or arena-backed row reads** - 53-68% of `terms_for` on high-row
   surfaces, ~0 on the median one. Backlog 38.
3. **Deconjugation memoisation** - 228 µs, 23% of the p50 hover, pure CPU with no
   database involved. Backlog 39.
4. **Gloss representation** - 0.6% of a hover. Choose it for the cache and the CSS
   probe, which is what the arena parser now does.

The schema, the indexes, and the statement cache need no work.

## What this does not measure

- Grouping and sorting are timed against a **replica** of `engine.rs:135-175`, because
  `Candidate`, `score` and `is_better` are private. Same `GroupKey`, same two `String`
  clones per row, same score formula. Everything else is the real code.
- One thread, warm cache, one corpus, one machine. A cold first hover pays page faults
  no repeat sweep sees.
- The bloom hash is FNV-1a with Kirsch-Mitzenmacher double hashing; hoshidicts uses
  xxHash, which is faster on short keys, so the probe cost is a ceiling and the
  benefit a floor.
- The arena prototypes are not the parser that must ship: no depth cap, no
  per-row degradation on malformed input, no editorial-role classification, no CSS
  matching.
