#!/usr/bin/env python3
"""Real hover payloads, extracted from a corpus of Yomitan dictionaries.

Answers the input half of "what would it cost to parse structured content at
hover time instead of at build time": for a realistic set of hover targets,
how many term-bank rows does one hover touch, and how many bytes of glossary
JSON are behind them.

The sample is the top N terms by frequency rank, because a frequent word is
the realistic hover worst case - it is the one that matches in the most
dictionaries. A row matches a sampled term when its expression (field 0) OR
its reading (field 1) equals the term, which is exactly what chibipop's
`term.surface` index resolves: `build.rs` writes one row keyed by the reading
and, when it differs, a second keyed by the written form.

Every count is reported twice: over all archives in the corpus (the upper
bound, a user who imported everything) and over LIBRARY alone (a realistic
monolingual-leaning library). `examples/hover_parse_bench.rs` reads the
LIBRARY list back out of `summary.json`, so the temp multi-dictionary build
and these numbers cannot drift apart.

Stdlib only. No setup step.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import zipfile
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

# A realistic monolingual-leaning library: Jitendex for the JA-EN spine, the
# big JA-JA kokugo dictionaries a monolingual reader actually stacks, one
# kanji dictionary, one thesaurus, one JA-EN. `hover_parse_bench build` reads
# this list back out of summary.json and builds a throwaway database from
# exactly these archives, so the "realistic library" column and the
# multi-dictionary hover baseline describe the same shelf.
LIBRARY = [
    "[JA-EN] jitendex-yomitan (2026-08-11).zip",
    "[JA-EN] 新和英.zip",
    "[JA-JA] 大辞林　第四版.zip",
    "[JA-JA] 大辞泉 第二版[2025-04-29].zip",
    "[JA-JA] 広辞苑 第七版.zip",
    "[JA-JA] 三省堂国語辞典　第八版.zip",
    "[JA-JA] 明鏡国語辞典 第三版[2025-08-18].zip",
    "[JA-JA] 新明解国語辞典　第八版.zip",
    "[JA-JA] 岩波国語辞典　第八版.zip",
    "[JA-JA] 精選版 日本国語大辞典.zip",
    "[JA-JA] 字通［普及版］.zip",
    "[JA-JA Thesaurus] 使い方の分かる 類語例解辞典 [2024-05-02].zip",
]

# Rank source. The fallback is used when the preferred archive cannot be read
# at all; a partially readable archive is an error worth seeing.
FREQ_PREFERRED = "[JA Freq] BCCWJ_SUW_LUW_combined.zip"
FREQ_FALLBACK = "[JA Freq] jiten_freq_global (2026-08-27).zip"


# ---- frequency ranks ----


def rank_of(payload) -> int | None:
    """A Yomitan `freq` payload's rank. Number, string, or object."""
    if isinstance(payload, bool):
        return None
    if isinstance(payload, (int, float)):
        return int(payload)
    if isinstance(payload, str):
        m = re.search(r"\d+", payload.replace(",", ""))
        return int(m.group()) if m else None
    if isinstance(payload, dict):
        # `{reading, frequency}`, `{value, displayValue}`, and the nested
        # `{reading, frequency: {value, displayValue}}` all appear in the wild.
        inner = payload.get("frequency", payload.get("value"))
        if isinstance(inner, dict):
            inner = inner.get("value", inner.get("frequency"))
        return rank_of(inner) if inner is not None else None
    return None


def top_terms(archive: Path, count: int) -> list[str]:
    """The `count` best-ranked terms in a frequency archive, best first."""
    best: dict[str, int] = {}
    with zipfile.ZipFile(archive) as z:
        banks = sorted_banks(z.namelist(), "term_meta_bank_")
        if not banks:
            raise ValueError(f"{archive.name}: no term_meta_bank_*.json")
        for bank in banks:
            for row in json.loads(z.read(bank)):
                if not (isinstance(row, list) and len(row) >= 3 and row[1] == "freq"):
                    continue
                term = row[0]
                rank = rank_of(row[2])
                if rank is None or not isinstance(term, str) or not term:
                    continue
                if term not in best or rank < best[term]:
                    best[term] = rank
    if not best:
        raise ValueError(f"{archive.name}: no usable freq rows")
    return sorted(best, key=lambda t: (best[t], t))[:count]


def read_ranks(corpus: Path, count: int) -> tuple[list[str], str]:
    for name in (FREQ_PREFERRED, FREQ_FALLBACK):
        path = corpus / name
        if not path.exists():
            continue
        try:
            return top_terms(path, count), name
        except Exception as exc:  # the fallback exists for exactly this
            print(f"payload: {name}: {type(exc).__name__}: {exc}", file=sys.stderr)
    raise SystemExit(f"payload: no readable frequency archive under {corpus}")


# ---- the pass over term banks ----


def sorted_banks(names, prefix: str) -> list[str]:
    """Bank entries, ordered numerically the way archive.rs orders them."""
    banks = [n for n in names if re.fullmatch(rf"{prefix}\d+\.json", os.path.basename(n))]
    return sorted(banks, key=lambda n: int(re.search(r"\d+", os.path.basename(n)).group()))


def collect(job: tuple[str, list[str]]) -> dict:
    """One archive: every glossary whose row matches a sampled term."""
    path, terms = job
    wanted = set(terms)
    out = {
        "path": path,
        "title": None,
        "banks": 0,
        "rows": 0,
        "matched": 0,
        "bytes": 0,
        "payloads": {},
        "error": None,
    }
    try:
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            if "index.json" in names:
                out["title"] = json.loads(z.read("index.json")).get("title")
            banks = sorted_banks(names, "term_bank_")
            out["banks"] = len(banks)
            for bank in banks:
                for row in json.loads(z.read(bank)):
                    if not isinstance(row, list) or len(row) < 6:
                        continue
                    out["rows"] += 1
                    expression = row[0] if isinstance(row[0], str) else ""
                    reading = row[1] if isinstance(row[1], str) else ""
                    if expression in wanted:
                        term = expression
                    elif reading in wanted:
                        term = reading
                    else:
                        continue
                    glossary = row[5]
                    if not isinstance(glossary, list) or not glossary:
                        continue
                    blob = json.dumps(glossary, ensure_ascii=False)
                    out["matched"] += 1
                    out["bytes"] += len(blob.encode("utf-8"))
                    out["payloads"].setdefault(term, []).append(blob)
    except Exception as exc:  # one bad archive must not lose the run
        out["error"] = f"{type(exc).__name__}: {exc}"
    return out


# ---- distribution ----


def percentile(values: list[int], q: float) -> int:
    """Nearest-rank percentile over an already-sorted list."""
    if not values:
        return 0
    i = min(len(values) - 1, max(0, int(round(q * (len(values) - 1)))))
    return values[i]


def describe(values: list[int]) -> dict:
    ordered = sorted(values)
    n = len(ordered)
    return {
        "n": n,
        "mean": round(sum(ordered) / n, 1) if n else 0.0,
        "p50": percentile(ordered, 0.50),
        "p95": percentile(ordered, 0.95),
        "p99": percentile(ordered, 0.99),
        "max": ordered[-1] if n else 0,
    }


# ---- main ----


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "corpus",
        type=Path,
        nargs="?",
        default=Path.home() / "Downloads" / "dict" / "Japanese",
        help="directory of Yomitan .zip archives",
    )
    ap.add_argument("--terms", type=int, default=2000, help="sample size (default 2000)")
    ap.add_argument("--worst", type=int, default=50, help="worst-case headwords (default 50)")
    ap.add_argument(
        "--budget-mb",
        type=float,
        default=150.0,
        help="raw payload bytes retained in the .jsonl (default 150)",
    )
    ap.add_argument("--out", type=Path, default=Path(__file__).parent / "results")
    ap.add_argument("--jobs", type=int, default=min(16, (os.cpu_count() or 4)))
    args = ap.parse_args()

    archives = sorted(str(p) for p in args.corpus.glob("*.zip"))
    if not archives:
        print(f"payload: no .zip archives under {args.corpus}", file=sys.stderr)
        return 1

    missing = [n for n in LIBRARY if not (args.corpus / n).exists()]
    if missing:
        print("payload: LIBRARY archives absent from the corpus:", file=sys.stderr)
        for n in missing:
            print(f"  {n}", file=sys.stderr)
        return 1

    started = time.time()
    sample, freq_source = read_ranks(args.corpus, args.terms)
    rank_of_term = {t: i for i, t in enumerate(sample)}
    print(f"payload: {len(sample)} sampled terms from {freq_source}", file=sys.stderr)

    # Library archives first: a term's payload list is written library-first
    # so `lib_rows` can name a prefix of it instead of a second copy.
    lib_paths = [str(args.corpus / n) for n in LIBRARY]
    lib_set = set(lib_paths)
    ordered = lib_paths + [p for p in archives if p not in lib_set]
    jobs = [(p, sample) for p in ordered]

    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(collect, jobs))
    scanned = time.time() - started

    # Aggregate. `all_*` spans every archive; `lib_*` spans LIBRARY only.
    payloads: dict[str, list[str]] = {t: [] for t in sample}
    lib_rows = dict.fromkeys(sample, 0)
    lib_bytes = dict.fromkeys(sample, 0)
    all_rows = dict.fromkeys(sample, 0)
    all_bytes = dict.fromkeys(sample, 0)
    for res in results:
        in_library = res["path"] in lib_set
        for term, blobs in res["payloads"].items():
            payloads[term].extend(blobs)
            size = sum(len(b.encode("utf-8")) for b in blobs)
            all_rows[term] += len(blobs)
            all_bytes[term] += size
            if in_library:
                lib_rows[term] += len(blobs)
                lib_bytes[term] += size

    matched = [t for t in sample if all_rows[t] > 0]
    lib_matched = [t for t in sample if lib_rows[t] > 0]

    # The worst case: most glossary bytes, plus most term-bank rows. Both are
    # measured over the whole corpus, which is the upper bound of the two.
    by_bytes = sorted(matched, key=lambda t: -all_bytes[t])[: args.worst]
    by_rows = sorted(matched, key=lambda t: -all_rows[t])[: args.worst]
    worst = set(by_bytes) | set(by_rows)

    # Retention. Both samples must survive the budget, so each gets its own
    # share: the worst-case terms are the biggest payloads in the corpus and
    # would otherwise eat the whole file and leave the frequency sample
    # unmeasurable. A term is retained whole or not at all - truncating one
    # term's payload list would silently understate its hover cost, and a
    # skipped term is visible in `dropped_over_budget`.
    #
    # The frequency sample is visited in stratified sweeps rather than in
    # rank order, because payload size correlates hard with rank: retaining a
    # rank prefix would fill the file with the heaviest terms in the sample
    # and bias every percentile the bench reports upwards. Each sweep spans
    # the whole rank range, so any prefix of the visit order is a fair sample.
    budget = int(args.budget_mb * 1_000_000)
    worst_budget = int(budget * 0.4)
    rest = [t for t in matched if t not in worst]
    rest_bytes = sum(all_bytes[t] for t in rest)
    stride = max(1, -(-rest_bytes // max(1, budget - worst_budget)))
    swept = [rest[i] for off in range(stride) for i in range(off, len(rest), stride)]
    plan = [
        (sorted(worst, key=lambda t: -all_bytes[t]), worst_budget),
        (swept, budget),
    ]

    args.out.mkdir(parents=True, exist_ok=True)
    jsonl = args.out / "hover-payloads.jsonl"
    retained, dropped, spent = 0, 0, 0
    kept = []
    with jsonl.open("w", encoding="utf-8") as fh:
        for terms, cap in plan:
            share, share_n = 0, 0
            for term in terms:
                size = all_bytes[term]
                if share + size > cap or spent + size > budget:
                    dropped += 1
                    continue
                share += size
                share_n += 1
                spent += size
                retained += 1
                fh.write(
                    json.dumps(
                        {
                            "term": term,
                            "rank": rank_of_term[term],
                            "worst": term in worst,
                            "rows": all_rows[term],
                            "bytes": all_bytes[term],
                            "lib_rows": lib_rows[term],
                            "lib_bytes": lib_bytes[term],
                            # Library archives were scanned first, so the
                            # first `lib_rows` payloads are the library ones.
                            "payloads": payloads[term],
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
            kept.append(share_n)

    failed = [r for r in results if r["error"]]
    summary = {
        "corpus": str(args.corpus),
        "freq_source": freq_source,
        "elapsed_s": round(time.time() - started, 1),
        "scan_s": round(scanned, 1),
        "archives": len(results),
        "archives_failed": [
            {"path": os.path.basename(r["path"]), "error": r["error"]} for r in failed
        ],
        "library": LIBRARY,
        "library_titles": [
            r["title"] for r in results if r["path"] in lib_set
        ],
        "sampled": len(sample),
        "matched_all": len(matched),
        "matched_library": len(lib_matched),
        "retained": retained,
        "retained_worst": kept[0],
        "retained_frequency": kept[1],
        "rank_stride": stride,
        "dropped_over_budget": dropped,
        "budget_bytes": budget,
        "retained_payload_bytes": spent,
        "jsonl_bytes": jsonl.stat().st_size,
        "all_corpus": {
            "rows": describe([all_rows[t] for t in matched]),
            "bytes": describe([all_bytes[t] for t in matched]),
        },
        "library_only": {
            "rows": describe([lib_rows[t] for t in lib_matched]),
            "bytes": describe([lib_bytes[t] for t in lib_matched]),
        },
        "worst_case": {
            "n": len(worst),
            "terms": [
                {
                    "term": t,
                    "rows": all_rows[t],
                    "bytes": all_bytes[t],
                    "lib_rows": lib_rows[t],
                    "lib_bytes": lib_bytes[t],
                }
                for t in sorted(worst, key=lambda t: -all_bytes[t])
            ],
            "rows": describe([all_rows[t] for t in worst]),
            "bytes": describe([all_bytes[t] for t in worst]),
        },
    }
    (args.out / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    print(
        f"{len(results)} archives in {summary['elapsed_s']:.0f}s -> {jsonl} "
        f"({summary['jsonl_bytes'] / 1e6:.0f} MB, {retained}/{len(sample)} terms retained)"
    )
    if failed:
        print(f"{len(failed)} failed:", file=sys.stderr)
        for r in failed:
            print(f"  {os.path.basename(r['path'])}: {r['error']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
