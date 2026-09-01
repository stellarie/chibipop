# Pitch payload census

Self-contained harness behind
[docs/research/pitch-accent-shapes.md](../../docs/research/pitch-accent-shapes.md). It
reads every `term_meta_bank_*.json` row in a corpus of Yomitan archives, counts
the three term-meta modes the schema defines, and walks the `pitch` payloads in
detail: accents per reading, how often the optional `nasal`, `devoice` and
`tags` fields appear, which form `position` takes, how long a marked reading
gets, and whether two pitch dictionaries agree about the same reading.

It exists because the Yomitan pitch schema permits far more than any archive
emits. The schema allows a mora-by-mora `HL` string for the downstep, a scalar
*or* a list for each mora marker, and a tag list on every accent; the only way
to learn which of those a real dictionary writes is to count. Ranking the work
by what the schema *permits* sizes it wrong, in exactly the way
[dict-census](../dict-census/README.md) already demonstrated for glossaries.

The pitch role is detected from bank content, never from the filename: an
archive has the pitch role when a `term_meta_bank_` row carries `"pitch"` in
field 1. That is the predicate the dictionary-roles spec uses, and in this
library it disagrees with the filename for one archive.

Stdlib Python only. No setup step, no downloads, nothing installed on the host.

## Run

```sh
python3 tools/pitch-census/census.py ~/.local/share/chibipop/library
python3 tools/pitch-census/report.py
```

`census.py` writes `results/census.json`; `report.py` aggregates it into
`results/tables.md`, the markdown embedded in the results doc. Both paths are
overridable with `--out` / `--in`.

The corpus argument is any directory of `.zip` archives - a chibipop library
directory, or a download folder. Every row is read; there is no sampling cap,
because the whole pitch corpus in this library is 49 MB uncompressed and the run
takes two seconds.

The corpus is **not** committed.

## Bank discovery matches chibipop's, deliberately

An entry name that starts with `term_meta_bank_` and ends with `.json`, at the
root of the zip - the rule `sorted_banks` (`src/dict/archive.rs:509`) applies,
which is stricter than Yomitan's `/^term_meta_bank_(\d+)\.json$/` in one
direction and looser in another. Names one directory deep are counted under
`banks_nested` and skipped, because chibipop cannot see them either. A number
this tool reports is therefore a number chibipop could reach.

## A bad CRC-32 is recorded, not fatal

Five of this library's pitch archives store a CRC-32 that does not match their
own payload. `ZipFile.read` refuses them outright, so the census would have no
data at all for the entire pitch corpus. `inflate_member` bypasses the check and
`crc_mismatch` records every member it had to bypass, so no count is quietly
based on unverified bytes. A length mismatch is still an error - that is
corruption rather than a bad checksum.

## Yomitan's own arithmetic, ported rather than reinvented

`kana_morae`, `downstep_positions` and `as_positions` are transliterations of
`getKanaMorae`, `getDownstepPositions` (`ext/js/language/ja/japanese.js`) and
`Translator._toNumberArray` (`ext/js/language/translator.js`). So a mora index
this tool counts is the index Yomitan counts, and a scalar marker and a
one-element list are the same fact in both.

## What it deliberately does not do

It counts payloads; it does not render them. It can say that the widest reading
in the corpus is 19 morae with four distinct accents, not that the resulting
card header fits.

It compares accents; it does not adjudicate them. Where two dictionaries
disagree it reports the disagreement, never which one is right.

It reads the pitch role's banks only. Term banks, frequency payload shapes and
`styles.css` belong to [dict-census](../dict-census/README.md).
