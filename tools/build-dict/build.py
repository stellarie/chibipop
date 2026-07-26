"""Build chibipop.sqlite from Yomitan format-3 archives."""

import argparse
import json
import sqlite3
import sys
from pathlib import Path

from flatten import flatten_glossary
from freq import lookup_freq, parse_freq_rows
from schema import create_schema
from yomitan import iter_freq_rows, iter_terms, read_index


def build(term_archives, freq_archives, out_path):
    """term_archives: [(Path, priority)]. Returns {'entries':n,'terms':n}."""
    out_path = Path(out_path)
    if out_path.exists():
        out_path.unlink()

    freq_table = {}
    for fa in freq_archives:
        freq_table.update(parse_freq_rows(iter_freq_rows(Path(fa))))

    conn = sqlite3.connect(out_path)
    create_schema(conn)

    entry_id = 0
    term_rows = 0

    for dict_id, (archive, priority) in enumerate(term_archives, start=1):
        archive = Path(archive)
        title = read_index(archive).get("title", archive.stem)
        conn.execute(
            "INSERT INTO dict (dict_id, name, priority) VALUES (?, ?, ?)",
            (dict_id, title, priority))

        entries, terms = [], []
        for t in iter_terms(archive):
            glosses = flatten_glossary(t.glossary)
            if not glosses:
                continue
            entry_id += 1
            senses = [{"glosses": glosses,
                       "pos": t.rules.split() if t.rules else [],
                       "misc": []}]
            entries.append(
                (entry_id, dict_id, json.dumps(senses, ensure_ascii=False)))

            written = t.term
            reading = t.reading or t.term
            rank = lookup_freq(freq_table, written, reading)

            # Reading row. `written` is NULL when the headword is kana-only.
            terms.append((reading, None if written == reading else written,
                          reading, t.rules, rank, entry_id, dict_id))
            # Written row, only when it differs.
            if written != reading:
                terms.append(
                    (written, written, reading, t.rules, rank, entry_id,
                     dict_id))

            if len(entries) >= 5000:
                _flush(conn, entries, terms)
                term_rows += len(terms)
                entries, terms = [], []

        _flush(conn, entries, terms)
        term_rows += len(terms)

    _write_meta(conn, term_archives, freq_archives)
    conn.commit()
    conn.execute("ANALYZE")
    conn.commit()
    conn.close()
    return {"entries": entry_id, "terms": term_rows}


def _write_meta(conn, term_archives, freq_archives):
    """Record provenance, per spec section 5: built_at and source hashes."""
    import datetime
    import hashlib

    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at', ?)",
        (datetime.datetime.now(datetime.timezone.utc)
         .replace(microsecond=0).isoformat(),))

    sources = []
    for path in [a for a, _ in term_archives] + list(freq_archives):
        path = Path(path)
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        sources.append({"name": path.name,
                        "bytes": path.stat().st_size,
                        "sha256": h.hexdigest()})
    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('source_hashes', ?)",
        (json.dumps(sources, ensure_ascii=False),))


def _flush(conn, entries, terms):
    if entries:
        conn.executemany(
            "INSERT INTO entry (entry_id, dict_id, senses) VALUES (?, ?, ?)",
            entries)
    if terms:
        conn.executemany(
            "INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)", terms)


def main(argv=None):
    # Archive filenames can contain characters a stock Windows console's
    # cp1252 stdout can't encode (e.g. "大辞林"); force UTF-8 so printing
    # them doesn't crash before build() is ever called.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")

    ap = argparse.ArgumentParser(description="Build chibipop.sqlite")
    ap.add_argument("--dicts-dir", type=Path, required=True,
                    help="directory containing Yomitan .zip archives")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args(argv)

    archives = sorted(args.dicts_dir.glob("*.zip"))
    if not archives:
        print(f"no .zip archives in {args.dicts_dir}", file=sys.stderr)
        return 1

    terms, freqs = [], []
    for a in archives:
        idx = read_index(a)
        if idx.get("frequencyMode") or "Freq" in a.name:
            freqs.append(a)
        else:
            terms.append(a)

    # Lower priority number sorts first in ranking ties.
    ranked = [(a, i) for i, a in enumerate(terms)]
    for a, p in ranked:
        print(f"term dict  [{p}] {a.name}")
    for a in freqs:
        print(f"freq dict      {a.name}")

    counts = build(ranked, freqs, args.out)
    print(f"wrote {args.out}: {counts['entries']} entries, "
          f"{counts['terms']} term rows")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
