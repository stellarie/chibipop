# Hover parse benchmark

Self-contained harness behind the "parse structured content at build time or
at hover time" question in the dictionary render-parity spec. It extracts the
glossary payloads a real hover touches, then times two things against them:
what a hover costs today, and what parsing the structured content per hover
would add.

It exists because the spec closes that question on an unmeasured assertion -
"hover runs roughly 25 point queries and cannot afford a parse" - and the
assertion decides whether a parser fix costs a schema bump and a full
dictionary rebuild or costs nothing. Two competing pieces of evidence say the
opposite: Hoshi Reader runs `JSON.parse` per glossary row per hover on an
iPhone, and chibipop's own `SqliteDictionary::entries` already parses JSON per
matched entry on every hover today. Only a measurement settles it.

The question is settled - the record keeps the raw glossary JSON and the tree
is parsed per hover - so `parse` now reports two rows: the
`serde_json::Value` baseline the verdict was stated against, and
`GlossDoc::parse`, the typed parser that shipped. Same payloads, same run.

Stdlib Python for the extraction, the `chibipop` crate itself for the timing.
No setup step, nothing installed on the host.

## Run

```sh
python3 tools/hover-parse-bench/payload.py ~/Downloads/dict/Japanese
cargo run -p chibipop --release --example hover_parse_bench -- parse
cargo run -p chibipop --release --example hover_parse_bench -- hover
```

`payload.py` writes `results/hover-payloads.jsonl` and `results/summary.json`.
The corpus argument is optional and defaults to `~/Downloads/dict/Japanese`.

Release mode is mandatory. A debug-profile `serde_json` number is off by an
order of magnitude and would answer the question backwards. `-p chibipop`
scopes the invocation to the root package: the two bin crates both produce a
binary named `chibipop`, so an unscoped command races two linkers over one
output path (see the comment block at the top of `Cargo.toml`).

The multi-dictionary baseline needs a database first. Build a throwaway one
into a scratch directory, time against it, then delete it:

```sh
TMPD=$(mktemp -d -p /var/tmp chibipop-bench.XXXXXX)
cargo run -p chibipop --release --example hover_parse_bench -- build "$TMPD/multi.sqlite"
cargo run -p chibipop --release --example hover_parse_bench -- hover "$TMPD/multi.sqlite" multi
rm -rf "$TMPD"
```

`build` refuses any output path under `~/.local/share/chibipop`. Use
`/var/tmp`, not `/tmp`: the database is a few gigabytes and `/tmp` is usually
a tmpfs, so a build there is paid for in RAM.

## The sample is frequent words, not random ones

`payload.py` samples the top 2000 terms by rank out of
`[JA Freq] BCCWJ_SUW_LUW_combined.zip`, falling back to
`[JA Freq] jiten_freq_global` when that archive cannot be read. A frequent
word is the realistic hover worst case, because it is the word that matches
in the most dictionaries. Sampling uniformly would fill the file with
hapaxes that match once and cost nothing.

A term-bank row matches a sampled term when its expression (field 0) **or**
its reading (field 1) equals the term. That is exactly what chibipop's
`term.surface` index resolves: `build.rs` writes one row keyed by the reading
and, when the written form differs, a second keyed by the written form. So a
kana sample term such as `こう` legitimately pulls in every homophone
headword, which is what makes it expensive.

On top of that sample the tool marks a **worst case** set: the 50 headwords
with the most glossary bytes, unioned with the 50 with the most term-bank
rows. Both benchmarks report that set on its own line. It is never folded
into the frequency sample, because it is deliberately over-represented in the
retained file and would drag the frequency sample's p95 and p99 up to the
worst case's median.

## `top 10` is the row that matters

Both benchmarks print a `top 10` line, and it is the one to read.
`LookupEngine::run` sorts its candidates and calls
`ranked.truncate(MAX_RESULTS)` **before** it calls `entries`
(`src/lookup/engine.rs:175-179`, `MAX_RESULTS = 10`). No hover deserializes
more than ten entries, however many rows the surface matched. The `every
match` and `every ... row` lines are the ceiling if that cap were lifted, not
what a hover pays.

The bench imports `MAX_RESULTS` from the crate rather than restating it, so
the line follows the constant.

Which ten is a model, not the ranking. `payload.py` writes payloads in
library-priority order, which is the order the engine's `dict_id`-ascending
tiebreak produces for one surface form at equal match length and equal
headword frequency; the real ranking also scores across written forms. The
count is exact, and the count is what bounds the cost.

## Every number is reported twice

Once over all archives in the corpus, and once over `LIBRARY` alone - twelve
archives that stand in for a realistic monolingual-leaning shelf. The
all-archives column is the upper bound: a user who imported everything. The
library column is what the verdict is stated against.

`hover_parse_bench build` reads that library list back out of
`results/summary.json` instead of keeping its own copy, so the "realistic
library" column and the multi-dictionary database it is compared against
always describe the same twelve dictionaries.

## Retention, and why terms go missing

The full sample is about 430 MB of glossary JSON. `payload.py` keeps
`--budget-mb` of it (150 MB by default, which lands the `.jsonl` at about
178 MB) and records `retained` against `sampled` in `summary.json`. Every
distribution in `summary.json` is computed over the **whole** sample, so the
budget changes what the Rust side can time, never what the distribution says.

A term is retained whole or not at all. Truncating one headword's payload
list would silently understate that headword's hover cost, which is the one
number the benchmark exists to produce.

The frequency sample is retained in stratified sweeps rather than in rank
order. Payload size correlates hard with rank, so keeping a rank prefix would
fill the file with the heaviest terms in the sample and bias every percentile
upwards.

## The live database is not the baseline

`hover` defaults to `~/.local/share/chibipop/chibipop.sqlite`, and on a
typical dev box that database holds **one** dictionary. Timing against it
measures the cheapest possible hover. The mode prints the dictionary count and
every dictionary name above its table so the number cannot be quoted out of
context; the multi-dictionary baseline is the `build`-then-`hover` pair above.

Reads are read-only by construction. `hover` opens through
`SqliteDictionary::open`, which passes `SQLITE_OPEN_READ_ONLY` without
`SQLITE_OPEN_CREATE` and runs no migration, and the second connection that
weighs the `glossary` column uses the same flags. Opening any WAL database does
touch its `-shm` sidecar; the database file itself is untouched.

## Layout

- `payload.py` — the extraction. One process per archive,
  `ProcessPoolExecutor`. A failing archive records its error and never fails
  the run.
- `../../examples/hover_parse_bench.rs` — the timing harness, in the root
  crate so it links the real `SqliteDictionary` and the real builder rather
  than a copy of them.
- `results/` — generated, gitignored.

## What it deliberately does not measure

Both `parse` rows include a full walk of the parsed tree, so the optimizer
cannot delete the parse and both numbers include traversal work. The walk is
not free and is not subtracted: read the rows against each other, not as an
absolute parse cost.

The `GlossDoc` row does not measure the parsed-tree cache
`SqliteDictionary` keeps, only one cold parse per payload. What a cached
entry costs in allocations and retained heap is
`examples/gloss_doc_alloc.rs`.

It also times one `terms_for` plus one `entries`, not the roughly 25 point
queries a full deconjugation fan-out issues. The parse cost it is compared
against is per hover either way - `entries` is called once, over the union of
every candidate's entry ids - but the query side of the baseline is therefore
a floor, not a full hover.

And it measures a warm cache. The first hover after a cold boot pays page
faults that no repeat sweep can see.
