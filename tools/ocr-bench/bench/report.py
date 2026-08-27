"""Aggregate results/*.json into markdown tables (results/tables.md).

The published report (docs/research/ocr-benchmark-results.md) embeds these
tables; regenerate with `.venv/bin/python -m bench.report`.
"""

from __future__ import annotations

import json
from collections import defaultdict

from bench.common import RESULTS

CONFIGS = ["meiki", "ppocrv5", "tess-fast", "tess-best", "manga-greedy", "manga-beam"]
SLICES = ["smoke", "horizontal", "mixed", "small", "vertical"]
LAT_IDS = ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"]


def load() -> dict[str, dict]:
    out = {}
    for cfg in CONFIGS:
        p = RESULTS / f"{cfg}.json"
        if p.exists():
            out[cfg] = json.loads(p.read_text())
    return out


def rows_of(res: dict) -> list[tuple[str, list[dict]]]:
    """Split a config into presentation rows (manga: one per feed)."""
    acc = res["accuracy"]
    feeds = sorted({r["feed"] for r in acc})
    if len(feeds) == 1:
        return [(res["config"], acc)]
    return [
        (f"{res['config']} ({feed})", [r for r in acc if r["feed"] == feed])
        for feed in feeds
    ]


def mean(vals: list[float]) -> float | None:
    return round(sum(vals) / len(vals), 3) if vals else None


def fmt(v, pct: bool = False) -> str:
    if v is None:
        return "—"
    return f"{v * 100:.1f}" if pct else f"{v}"


def table(header: list[str], body: list[list[str]]) -> str:
    lines = ["| " + " | ".join(header) + " |",
             "|" + "|".join("---" for _ in header) + "|"]
    lines += ["| " + " | ".join(r) + " |" for r in body]
    return "\n".join(lines) + "\n"


def main() -> None:
    results = load()
    cold = json.loads((RESULTS / "cold.json").read_text()) if (RESULTS / "cold.json").exists() else {}
    md: list[str] = []

    # ---------------------------------------------------------- CER by slice
    md.append("## CER (%) by corpus slice — unmasked crops, lower is better\n")
    for scale in (1, 2):
        body = []
        for cfg, res in results.items():
            for label, acc in rows_of(res):
                row = [label]
                for sl in SLICES:
                    vals = [r["cer"] for r in acc
                            if r["slice"] == sl and r["scale"] == scale and not r["mask"]]
                    row.append(fmt(mean(vals), pct=True))
                body.append(row)
        md.append(f"### {scale}x scale\n")
        md.append(table(["config", *SLICES], body))

    # ------------------------------------------------------------- hit-scan
    md.append("\n## Hit-scan success (%) — cursor at each GT char center, unmasked\n")
    for scale in (1, 2):
        body = []
        for cfg, res in results.items():
            for label, acc in rows_of(res):
                row = [label]
                for sl in SLICES:
                    hits = sum(r.get("hits") or 0 for r in acc
                               if r["slice"] == sl and r["scale"] == scale and not r["mask"]
                               and r.get("total"))
                    total = sum(r.get("total") or 0 for r in acc
                                if r["slice"] == sl and r["scale"] == scale and not r["mask"])
                    row.append(f"{hits / total * 100:.1f}" if total else "—")
                body.append(row)
        md.append(f"### {scale}x scale\n")
        md.append(table(["config", *SLICES], body))

    # -------------------------------------------------------------- latency
    md.append("\n## Latency per crop size (ms, CPU) — cold / warm p50 / warm p95\n")
    body = []
    for cfg, res in results.items():
        row = [cfg]
        for lid in LAT_IDS:
            lat = res["latency"].get(lid)
            c = cold.get(cfg, {}).get(lid, {}).get("cold_ms")
            if lat:
                cell = f"{c or '—'} / {lat['p50_ms']} / {lat['p95_ms']}"
                if lat.get("reduced"):
                    cell += f" (n={lat['n']})"
            else:
                cell = "—"
            row.append(cell)
        body.append(row)
    md.append(table(["config", *LAT_IDS], body))

    # ------------------------------------------------- construction and RSS
    md.append("\n## Engine construction and memory\n")
    body = []
    for cfg, res in results.items():
        colds = [v["construction_ms"] for v in cold.get(cfg, {}).values()]
        body.append([
            cfg,
            f"{min(colds)}–{max(colds)}" if colds else str(res["construction_ms"]),
            str(res["peak_rss_mib"]),
        ])
    md.append(table(["config", "construction ms (min–max across cold runs)", "peak RSS MiB (full run)"], body))

    # --------------------------------------------------------- masked deltas
    md.append("\n## ADR-0008 masked variants — ΔCER vs the unmasked 2x base (pp)\n")
    md.append("Positive = mask made it worse. `ΔCER-dropped` = same delta after "
              "removing predicted chunks whose boxes intersect the mask, i.e. "
              "what survives chibipop's layout.rs clipped-word exclusion "
              "(box engines only). `boxes-in-mask` = predicted chunks "
              "intersecting the mask rect; `ins` = inserted chars vs masked GT "
              "(both mean per crop; hallucination signals).\n")

    for cfg, res in results.items():
        for label, acc in rows_of(res):
            base_cer = {r["id"]: r["cer"] for r in acc if not r["mask"]}
            masked = [r for r in acc if r["mask"]]
            if not masked:
                continue
            md.append(f"\n### {label}\n")
            groups: dict[str, dict[str, list]] = {
                "position": defaultdict(list), "fill": defaultdict(list),
                "edge": defaultdict(list),
            }
            for r in masked:
                b = base_cer.get(r["mask"]["unmasked"])
                if b is None:
                    continue
                delta = r["cer"] - b
                d2 = r["cer_dropped"] - b if "cer_dropped" in r else None
                rec = (delta, d2, r.get("pred_boxes_in_mask"), r["ins"])
                groups["position"][r["mask"]["pos"]].append(rec)
                if r["mask"]["pos"] != "outside":
                    groups["fill"][r["mask"]["fill"]].append(rec)
                    groups["edge"][r["mask"]["edge"]].append(rec)
            for gname, g in groups.items():
                body = []
                for key, recs in sorted(g.items()):
                    deltas = [d for d, _, _, _ in recs]
                    dropped = [d2 for _, d2, _, _ in recs if d2 is not None]
                    boxes = [b for _, _, b, _ in recs if b is not None]
                    ins = [i for _, _, _, i in recs]
                    body.append([
                        key,
                        f"{mean(deltas) * 100:+.1f}",
                        f"{mean(dropped) * 100:+.1f}" if dropped else "—",
                        fmt(mean(boxes)) if boxes else "—",
                        fmt(mean(ins)),
                    ])
                md.append(f"**by {gname}**\n")
                md.append(table([gname, "ΔCER pp", "ΔCER-dropped pp", "boxes-in-mask", "ins"], body))

    fails = RESULTS / "failures.json"
    if fails.exists():
        md.append("\n## Failures\n```json\n" + fails.read_text() + "\n```\n")

    (RESULTS / "tables.md").write_text("\n".join(md))
    print("wrote results/tables.md")


if __name__ == "__main__":
    main()
