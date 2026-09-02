#!/usr/bin/env python3
"""Pitch-accent shape census over a corpus of Yomitan dictionaries.

Reads every `term_meta_bank_*.json` row in every archive, counts the three
term-meta modes the Yomitan schema defines - `freq`, `pitch`, `ipa` - and then
walks the `pitch` payloads in detail: how many accents a reading carries, how
often the optional `nasal`, `devoice` and `tags` fields appear, which form the
`position` field takes, how long a marked reading gets, and whether two pitch
dictionaries agree about the same reading.

It answers the questions a schema reading cannot: the schema permits a mora-by-mora
`HL` string for `position` and permits `nasal` and `devoice` on every accent, and
the only way to learn whether any dictionary emits them is to count.

The pitch role is detected from bank content, never from the filename - an
archive has the pitch role when a `term_meta_bank_` row carries `"pitch"` in
field 1.

Bank discovery mirrors chibipop's own `sorted_banks` (`src/dict/archive.rs:509`):
an entry name that starts with `term_meta_bank_` and ends with `.json`, at the
root of the zip. Names one directory deep are counted separately and skipped,
because chibipop cannot see them either.

Stdlib only. No setup step.
"""

from __future__ import annotations

import argparse
import collections
import itertools
import json
import os
import struct
import sys
import time
import zipfile
import zlib
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

BANK_PREFIX = "term_meta_bank_"

# The modes `dictionary-term-meta-bank-v3-schema.json` enumerates. Anything
# else is counted under `(unknown)`, because a row chibipop cannot classify is
# a fact about the corpus, not a parse error.
MODES = ("freq", "pitch", "ipa")

# `TermMetaPitchData` and its `pitches[]` element, from
# types/ext/dictionary-data.d.ts. The schema sets `additionalProperties: false`
# on both, so a key outside these sets is a payload Yomitan itself would
# refuse at import.
PAYLOAD_KEYS = frozenset(("reading", "pitches"))
ACCENT_KEYS = frozenset(("position", "nasal", "devoice", "tags"))

# Yomitan's own mora rule, `SMALL_KANA_SET` in ext/js/language/ja/japanese.js:
# a small kana joins the mora before it. Note what is absent: `ッ` and `ー`
# each count as a mora of their own.
SMALL_KANA = frozenset("ぁぃぅぇぉゃゅょゎァィゥェォャュョヮ")

# How many verbatim payloads to keep per category per archive. The doc quotes
# these; a handful is enough to show the shape and keeps census.json readable.
FIXTURES_PER_CATEGORY = 3


# ---- Yomitan's mora and downstep arithmetic, ported ----


def kana_morae(text: str) -> list[str]:
    """`getKanaMorae` (ext/js/language/ja/japanese.js), character for
    character, so a mora index counted here counts what Yomitan counts."""
    morae: list[str] = []
    for c in text:
        if c in SMALL_KANA and morae:
            morae[-1] += c
        else:
            morae.append(c)
    return morae


def downstep_positions(pitch_string: str) -> list[int]:
    """`getDownstepPositions` (same file): the `HL` form reduced to the mora
    indices where the pitch falls. `[0]` means heiban and `[-1]` means a string
    that never falls and never starts low, which has no downstep reading."""
    downsteps = [
        i for i in range(1, len(pitch_string))
        if pitch_string[i - 1] == "H" and pitch_string[i] == "L"
    ]
    if not downsteps:
        downsteps.append(0 if pitch_string.startswith("L") else -1)
    return downsteps


def as_positions(value: object) -> list[int]:
    """`Translator._toNumberArray` (ext/js/language/translator.js): a scalar
    marker and a one-element list mean the same thing, so both normalise to a
    list before anything counts them."""
    if isinstance(value, list):
        return [v for v in value if isinstance(v, int) and not isinstance(v, bool)]
    if isinstance(value, int) and not isinstance(value, bool):
        return [value]
    return []


def position_key(position: object) -> str:
    """One accent's downstep, as a comparable token.

    An `HL` string that reduces to exactly one downstep is written in the
    integer form, so an archive using the string form still compares against
    one using the integer form. A string with several falls cannot be reduced
    without losing information and keeps its own token."""
    if isinstance(position, bool):
        return f"?{position!r}"
    if isinstance(position, int):
        return f"d{position}"
    if isinstance(position, str):
        steps = downstep_positions(position)
        if len(steps) == 1 and steps[0] >= 0:
            return f"d{steps[0]}"
        return f"hl{position}"
    return f"?{position!r}"


def accent_key(accent: dict, with_marks: bool) -> str:
    """The token two dictionaries are compared on.

    `with_marks` is the strict form: two accents that name the same downstep
    but disagree about which mora is nasal are the same row in the card header
    today, and two different rows once the header draws the marks, so both
    counts are reported."""
    key = position_key(accent.get("position"))
    if not with_marks:
        return key
    nasal = ",".join(str(v) for v in sorted(as_positions(accent.get("nasal"))))
    devoice = ",".join(str(v) for v in sorted(as_positions(accent.get("devoice"))))
    return f"{key}|n{nasal}|v{devoice}"


# ---- per-archive walk ----


def new_stats(path: str) -> dict:
    return {
        "path": path,
        "file": os.path.basename(path),
        "title": None,
        "revision": None,
        "format": None,
        "banks": 0,
        "banks_nested": 0,
        "crc_mismatch": [],
        "rows": 0,
        "modes": collections.Counter(),
        # pitch rows only, from here down
        "pitch_rows": 0,
        "expressions": 0,
        "readings": 0,
        "pairs": 0,
        "pairs_multi_row": 0,
        "rows_multi_accent": 0,
        "rows_no_accent": 0,
        "rows_with_duplicate_accents": 0,
        "accents": 0,
        "max_accents_per_row": 0,
        "max_distinct_accents_per_pair": 0,
        "max_distinct_accents_pair": None,
        "distinct_accents_histogram": {},
        "max_position": 0,
        "max_morae": 0,
        "max_morae_reading": None,
        "mora_histogram": collections.Counter(),
        "accents_histogram": collections.Counter(),
        "accent_fields": collections.Counter(),
        "rows_with_field": collections.Counter(),
        "position_forms": collections.Counter(),
        "position_values": collections.Counter(),
        "mark_positions": collections.Counter(),
        "position_over_morae": 0,
        "reading_equals_expression": 0,
        "reading_empty": 0,
        "reading_not_kana": 0,
        "reading_odd_chars": collections.Counter(),
        "unknown_payload_keys": collections.Counter(),
        "unknown_accent_keys": collections.Counter(),
        "tag_values": collections.Counter(),
        "fixtures": collections.defaultdict(list),
        "records": {},
        "error": None,
    }


def inflate_member(z: zipfile.ZipFile, name: str) -> bytes:
    """One member's bytes, with the stored CRC-32 ignored.

    Five of this library's pitch archives declare a CRC-32 that does not match
    their own payload, so `ZipFile.read` refuses them outright and the census
    would otherwise have no data at all for them. The bytes themselves are
    intact - they inflate to exactly the declared uncompressed length and
    parse as JSON - so the check is bypassed and the archive is *recorded* as
    having failed it. A length mismatch is still an error, because that is
    corruption rather than a bad checksum.
    """
    info = z.getinfo(name)
    with open(z.filename, "rb") as f:
        f.seek(info.header_offset)
        name_len, extra_len = struct.unpack("<HH", f.read(30)[26:30])
        f.seek(info.header_offset + 30 + name_len + extra_len)
        raw = f.read(info.compress_size)
    if info.compress_type == zipfile.ZIP_STORED:
        data = raw
    elif info.compress_type == zipfile.ZIP_DEFLATED:
        data = zlib.decompress(raw, -15)
    else:
        raise zipfile.BadZipFile(f"{name}: compression method {info.compress_type}")
    if len(data) != info.file_size:
        raise zipfile.BadZipFile(
            f"{name}: inflated to {len(data)} bytes, header says {info.file_size}"
        )
    return data


def read_member(z: zipfile.ZipFile, name: str, st: dict) -> bytes:
    try:
        return z.read(name)
    except zipfile.BadZipFile as exc:
        if "Bad CRC-32" not in str(exc):
            raise
        st["crc_mismatch"].append(name)
        return inflate_member(z, name)


def keep(st: dict, category: str, row: object) -> None:
    bucket = st["fixtures"][category]
    if len(bucket) < FIXTURES_PER_CATEGORY:
        bucket.append(json.dumps(row, ensure_ascii=False))


def record(st: dict, category: str, row: object) -> None:
    """The row that currently holds a maximum. Overwrites rather than
    appends, because `keep` would fill its bucket with the first three
    records and drop the one that actually holds the bound."""
    st["records"][category] = json.dumps(row, ensure_ascii=False)


def is_kana(c: str) -> bool:
    return 0x3041 <= ord(c) <= 0x309F or 0x30A0 <= ord(c) <= 0x30FF


def walk_pitch_row(
    st: dict, row: list, seen: collections.Counter, maps: dict[str, dict[str, list[str]]]
) -> None:
    """One `[expression, "pitch", payload]` row into the counters, and its
    accent tokens into `maps` for the cross-dictionary comparison."""
    expression = row[0] if isinstance(row[0], str) else ""
    payload = row[2]
    if not isinstance(payload, dict):
        st["unknown_payload_keys"][f"(payload is {type(payload).__name__})"] += 1
        keep(st, "payload-not-an-object", row)
        return

    st["pitch_rows"] += 1
    for key in payload.keys() - PAYLOAD_KEYS:
        st["unknown_payload_keys"][key] += 1
        keep(st, "unknown-payload-key", row)

    reading = payload.get("reading")
    if not isinstance(reading, str):
        st["unknown_payload_keys"]["(reading is not a string)"] += 1
        keep(st, "reading-not-a-string", row)
        reading = ""
    if reading == "":
        st["reading_empty"] += 1
        keep(st, "reading-empty", row)
    elif reading == expression:
        st["reading_equals_expression"] += 1
    odd = [c for c in reading if not is_kana(c)]
    if odd:
        st["reading_not_kana"] += 1
        st["reading_odd_chars"].update(odd)
        keep(st, "reading-not-kana", row)

    morae = len(kana_morae(reading))
    st["mora_histogram"][morae] += 1
    if morae > st["max_morae"]:
        st["max_morae"] = morae
        st["max_morae_reading"] = reading
        record(st, "longest-reading", row)

    pitches = payload.get("pitches")
    if not isinstance(pitches, list):
        st["unknown_payload_keys"]["(pitches is not a list)"] += 1
        keep(st, "pitches-not-a-list", row)
        return

    st["accents"] += len(pitches)
    st["accents_histogram"][len(pitches)] += 1
    if len(pitches) > st["max_accents_per_row"]:
        st["max_accents_per_row"] = len(pitches)
        record(st, "most-accents-in-one-row", row)
    if len(pitches) == 0:
        st["rows_no_accent"] += 1
        keep(st, "no-accent-data", row)
    elif len(pitches) > 1:
        st["rows_multi_accent"] += 1
        keep(st, "multiple-accents", row)
    else:
        keep(st, "single-accent", row)

    fields_in_row: set[str] = set()
    for accent in pitches:
        if not isinstance(accent, dict):
            st["unknown_accent_keys"][f"(accent is {type(accent).__name__})"] += 1
            keep(st, "accent-not-an-object", row)
            continue
        for key in accent.keys() - ACCENT_KEYS:
            st["unknown_accent_keys"][key] += 1
            keep(st, "unknown-accent-key", row)
        for key in ACCENT_KEYS & accent.keys():
            st["accent_fields"][key] += 1
            fields_in_row.add(key)

        position = accent.get("position")
        if isinstance(position, bool) or not isinstance(position, (int, str)):
            st["position_forms"]["(neither integer nor string)"] += 1
            keep(st, "position-of-another-type", row)
        elif isinstance(position, int):
            st["position_forms"]["integer"] += 1
            st["position_values"][position] += 1
            if position > st["max_position"]:
                st["max_position"] = position
                record(st, "highest-position", row)
            if position == 0:
                keep(st, "heiban-position-0", row)
            if morae and position > morae:
                st["position_over_morae"] += 1
                keep(st, "position-past-the-last-mora", row)
        else:
            st["position_forms"]["HL string"] += 1
            st["position_values"][position] += 1
            keep(st, "position-as-an-HL-string", row)

        # A present `nasal` is not a nasal mora: all five of this library's
        # pitch dictionaries write `"nasal": []` on every accent, so the
        # number the mark drawing rests on is the *non-empty* one and the
        # two are counted apart.
        for key in ("nasal", "devoice"):
            if key not in accent:
                continue
            shape = "list" if isinstance(accent[key], list) else "scalar"
            st["accent_fields"][f"{key} ({shape})"] += 1
            marks = as_positions(accent[key])
            if not marks:
                st["accent_fields"][f"{key} (empty)"] += 1
                continue
            st["accent_fields"][f"{key} (non-empty)"] += 1
            fields_in_row.add(f"{key} (non-empty)")
            for mark in marks:
                st["mark_positions"][f"{key} {mark}"] += 1
            keep(st, f"{key}-marker-{shape}", row)
        tags = accent.get("tags")
        if isinstance(tags, list):
            keep(st, "tags-present", row)
            for tag in tags:
                st["tag_values"][tag if isinstance(tag, str) else repr(tag)] += 1

    for key in fields_in_row:
        st["rows_with_field"][key] += 1
    if any(key.endswith("(non-empty)") for key in fields_in_row):
        st["rows_with_field"]["nasal or devoice (non-empty)"] += 1

    distinct = {accent_key(a, False) for a in pitches if isinstance(a, dict)}
    if len(distinct) < sum(1 for a in pitches if isinstance(a, dict)):
        st["rows_with_duplicate_accents"] += 1
        keep(st, "one-row-repeating-an-accent", row)

    pair = (expression, reading)
    seen[pair] += 1
    if seen[pair] == 2:
        st["pairs_multi_row"] += 1
        keep(st, "second-row-for-one-expression-and-reading", row)

    key = f"{expression}\x1f{reading}"
    for accent in pitches:
        if isinstance(accent, dict):
            maps["loose"].setdefault(key, []).append(accent_key(accent, False))
            maps["strict"].setdefault(key, []).append(accent_key(accent, True))


def census(path: str) -> tuple[dict, dict[str, dict[str, list[str]]]]:
    """One archive's stats, plus its (expression, reading) -> accent tokens
    maps for the cross-dictionary comparison the parent does."""
    st = new_stats(path)
    maps: dict[str, dict[str, list[str]]] = {"loose": {}, "strict": {}}
    seen: collections.Counter = collections.Counter()
    try:
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            if "index.json" in names:
                index = json.loads(read_member(z, "index.json", st))
                st["title"] = index.get("title")
                st["revision"] = index.get("revision")
                st["format"] = index.get("format", index.get("version"))
            st["banks_nested"] = sum(
                1 for n in names
                if not n.startswith(BANK_PREFIX)
                and os.path.basename(n).startswith(BANK_PREFIX)
                and n.endswith(".json")
            )
            banks = sorted(
                n for n in names if n.startswith(BANK_PREFIX) and n.endswith(".json")
            )
            st["banks"] = len(banks)
            for bank in banks:
                rows = json.loads(read_member(z, bank, st))
                if not isinstance(rows, list):
                    continue
                for row in rows:
                    st["rows"] += 1
                    if not isinstance(row, list) or len(row) < 3:
                        st["modes"]["(malformed row)"] += 1
                        continue
                    mode = row[1] if isinstance(row[1], str) else "(mode not a string)"
                    st["modes"][mode if mode in MODES else f"(unknown) {mode}"] += 1
                    if mode == "pitch":
                        walk_pitch_row(st, row, seen, maps)
    except Exception as exc:  # one bad archive must not lose the run
        st["error"] = f"{type(exc).__name__}: {exc}"

    if st["pitch_rows"]:
        st["pairs"] = len(seen)
        st["expressions"] = len({e for e, _ in seen})
        st["readings"] = len({r for _, r in seen})
        # After merging the rows that name one (expression, reading) - 3 614
        # of them across this library do - how many *distinct* accents does
        # one dictionary end up claiming? This, unioned over the enabled
        # dictionaries, is what bounds the card-header row.
        distinct = collections.Counter()
        for key, tokens in maps["loose"].items():
            n = len(set(tokens))
            distinct[n] += 1
            if n > st["max_distinct_accents_per_pair"]:
                st["max_distinct_accents_per_pair"] = n
                st["max_distinct_accents_pair"] = key.replace("\x1f", " / ")
        st["distinct_accents_histogram"] = {str(k): distinct[k] for k in sorted(distinct)}

    for key in (
        "modes", "mora_histogram", "accents_histogram", "accent_fields",
        "rows_with_field", "position_forms", "position_values", "mark_positions",
        "unknown_payload_keys", "unknown_accent_keys", "tag_values",
        "reading_odd_chars",
    ):
        st[key] = {str(k): v for k, v in st[key].items()}
    st["fixtures"] = dict(st["fixtures"])
    return st, maps


# ---- cross-dictionary agreement ----


def agreement(maps: dict[str, dict[str, list[str]]]) -> dict:
    """How often the pitch dictionaries that both know a reading say the same
    thing about it.

    Compared as *sets* of accent tokens, because a dictionary listing the same
    accent twice is still making one claim. Three outcomes, and the middle one
    is the reason the two extremes are not enough: `identical` sets; `partial`,
    where at least two dictionaries share an accent and at least one names an
    accent the others do not; and `disjoint`, where no two dictionaries share
    a single accent and nothing can be deduplicated.
    """
    names = sorted(maps)
    per_pair: dict[str, dict] = {}
    totals = collections.Counter()
    union_histogram: collections.Counter = collections.Counter()
    examples: dict[str, list[str]] = collections.defaultdict(list)
    claims = 0
    distinct = 0

    keys = collections.Counter()
    for name in names:
        for key in maps[name]:
            keys[key] += 1

    for key, holders in keys.items():
        sets = [frozenset(maps[n][key]) for n in names if key in maps[n]]
        union = frozenset().union(*sets)
        union_histogram[len(union)] += 1
        if holders < 2:
            totals["one dictionary only"] += 1
            continue
        claims += sum(len(s) for s in sets)
        distinct += len(union)
        if len(set(sets)) == 1:
            outcome = "identical"
        elif any(
            n >= 2 for n in collections.Counter(t for s in sets for t in s).values()
        ):
            outcome = "partial"
        else:
            outcome = "disjoint"
        totals[outcome] += 1
        if len(examples[outcome]) < 6:
            expression, _, reading = key.partition("\x1f")
            examples[outcome].append(
                f"{expression} ({reading}): "
                + "; ".join(
                    f"{n}={sorted(maps[n][key])}" for n in names if key in maps[n]
                )
            )

    for a, b in itertools.combinations(names, 2):
        shared = maps[a].keys() & maps[b].keys()
        same = sum(1 for k in shared if frozenset(maps[a][k]) == frozenset(maps[b][k]))
        per_pair[f"{a} vs {b}"] = {
            "shared": len(shared), "identical": same, "differing": len(shared) - same,
        }

    return {
        "keys": len(keys),
        "totals": dict(totals),
        "pairwise": per_pair,
        "union_histogram": {str(k): v for k, v in sorted(union_histogram.items())},
        "max_accents_after_union": max(union_histogram, default=0),
        # Over the readings more than one dictionary knows: how many accent
        # claims arrive, and how many rows survive deduplication.
        "claims_multi_dictionary": claims,
        "distinct_multi_dictionary": distinct,
        "examples": dict(examples),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "corpus", type=Path,
        help="directory of Yomitan .zip archives, e.g. a chibipop library directory",
    )
    ap.add_argument(
        "--out", type=Path, default=Path(__file__).parent / "results" / "census.json",
    )
    ap.add_argument("--jobs", type=int, default=min(12, (os.cpu_count() or 4)))
    args = ap.parse_args()

    archives = sorted(str(p) for p in args.corpus.glob("*.zip"))
    if not archives:
        print(f"census: no .zip archives under {args.corpus}", file=sys.stderr)
        return 1

    started = time.time()
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(census, archives))
    elapsed = time.time() - started

    dictionaries = [st for st, _ in results]
    # Keyed by dictionary title, because that is the name a config list and
    # the popup use; only the archives that actually carry pitch rows take
    # part in the comparison.
    loose = {
        st["title"] or st["file"]: maps["loose"]
        for st, maps in results if maps["loose"]
    }
    strict = {
        st["title"] or st["file"]: maps["strict"]
        for st, maps in results if maps["strict"]
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(
            {
                "corpus": str(args.corpus),
                "elapsed_s": round(elapsed, 1),
                "dictionaries": dictionaries,
                "agreement": agreement(loose),
                "agreement_with_marks": agreement(strict),
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )

    pitch = [d for d in dictionaries if d["pitch_rows"]]
    failed = [d for d in dictionaries if d["error"]]
    print(
        f"{len(dictionaries)} archives in {elapsed:.0f}s, "
        f"{len(pitch)} with pitch rows -> {args.out}"
    )
    if failed:
        print(f"{len(failed)} failed:", file=sys.stderr)
        for d in failed:
            print(f"  {d['file']}: {d['error']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
