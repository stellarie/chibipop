"""Run one engine configuration in an isolated process (clean RSS/cold numbers).

    python -m bench.run_one --config meiki --out results/meiki.json
    python -m bench.run_one --config manga-greedy --cold j1_2x

Configs: meiki | ppocrv5 | tess-fast | tess-best | manga-greedy | manga-beam.
The meiki run additionally writes results/meiki_lines.json (detector line boxes
per crop) which the manga-* runs consume for feed (b).
"""

from __future__ import annotations

import argparse
import json
import time

from bench.common import (
    RESULTS,
    Box,
    boxes_intersect,
    cer,
    hit_scan,
    latency_stats,
    levenshtein_ops,
    load_crop,
    load_manifest,
    normalise,
    peak_rss_mib,
)

# One representative crop per production size (warm p50/p95 over >=100 iters).
LATENCY_IDS = ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"]
WARMUP = 3
N_WARM = 100
# Autoregressive beam decode has no KV cache in this export; keep a run under
# ~5 min per size by shrinking n when a single call is extremely slow. Any
# shrink is recorded in the output and called out in the report.
MAX_SECONDS_PER_SIZE = 300.0


def make_engine(config: str):
    if config == "meiki":
        from bench.eng_meiki import MeikiEngine

        return MeikiEngine()
    if config == "ppocrv5":
        from bench.eng_rapid import RapidEngine

        return RapidEngine()
    if config.startswith("tess-"):
        from bench.eng_tess import TessEngine

        return TessEngine(config.removeprefix("tess-"))
    if config.startswith("manga-"):
        from bench.eng_manga import MangaEngine

        return MangaEngine(config.removeprefix("manga-"))
    raise SystemExit(f"unknown config {config}")


def eval_crop(entry: dict, out, feed: str) -> dict:
    gt = normalise(entry["text"])
    pred = normalise(out.text)
    s, d, i = levenshtein_ops(gt, pred)
    rec = {
        "id": entry["id"],
        "slice": entry["slice"],
        "base": entry["base"],
        "scale": entry["scale"],
        "feed": feed,
        "mask": entry["mask"],
        "gt": gt,
        "pred": pred,
        "cer": round(cer(gt, pred), 4),
        "subs": s,
        "dels": d,
        "ins": i,
    }
    if out.boxes:
        hits, total = hit_scan(entry["chars"], out.boxes)
        rec["hits"], rec["total"] = hits, total
    elif entry["chars"]:
        rec["hits"] = rec["total"] = None  # no geometry (manga-ocr)
    if entry["mask"] and entry["mask"]["pos"] != "outside":
        mrect = entry["mask"]["rect"]
        # clip to crop
        x0, y0, mw, mh = mrect
        x1, y1 = min(x0 + mw, entry["w"]), min(y0 + mh, entry["h"])
        x0, y0 = max(x0, 0), max(y0, 0)
        clipped = (x0, y0, x1 - x0, y1 - y0)
        rec["pred_boxes_in_mask"] = sum(
            1 for b in out.boxes if normalise(b.text) and boxes_intersect(b, clipped)
        )
        if out.boxes:
            # Production-equivalent CER: chibipop's layout.rs drops words whose
            # rects intersect the mask (ADR-0008 "mask boundary is a capture
            # edge"), so boundary garbage with honest geometry is discarded.
            kept = "".join(b.text for b in out.boxes if not boxes_intersect(b, clipped))
            rec["cer_dropped"] = round(cer(gt, normalise(kept)), 4)
    return rec


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--out")
    ap.add_argument("--cold", help="crop id: construct + one call, print JSON, exit")
    args = ap.parse_args()

    manifest = load_manifest()
    crops = {e["id"]: e for e in manifest["crops"]}

    t0 = time.perf_counter()
    engine = make_engine(args.config)
    construction_ms = (time.perf_counter() - t0) * 1000.0

    if args.cold:
        entry = crops[args.cold]
        img = load_crop(entry)
        t0 = time.perf_counter()
        engine.recognize(img)
        cold_ms = (time.perf_counter() - t0) * 1000.0
        print(json.dumps({
            "config": args.config, "crop": args.cold,
            "construction_ms": round(construction_ms, 1),
            "cold_ms": round(cold_ms, 1),
        }))
        return

    is_manga = args.config.startswith("manga-")
    meiki_lines: dict = {}
    if is_manga:
        meiki_lines = json.loads((RESULTS / "meiki_lines.json").read_text())

    accuracy: list[dict] = []
    det_lines_out: dict[str, list] = {}
    for entry in manifest["crops"]:
        img = load_crop(entry)
        out = engine.recognize(img)
        accuracy.append(eval_crop(entry, out, "whole"))
        if args.config == "meiki":
            det_lines_out[entry["id"]] = engine.line_boxes(img)
        if is_manga:
            out_b = engine.recognize_lines(img, meiki_lines.get(entry["id"], []))
            accuracy.append(eval_crop(entry, out_b, "meiki-lines"))

    latency: dict[str, dict] = {}
    for lid in LATENCY_IDS:
        entry = crops[lid]
        img = load_crop(entry)
        for _ in range(WARMUP):
            engine.recognize(img)
        t0 = time.perf_counter()
        engine.recognize(img)
        est = time.perf_counter() - t0
        n = N_WARM
        if est * N_WARM > MAX_SECONDS_PER_SIZE:
            n = max(25, int(MAX_SECONDS_PER_SIZE / est))
        samples = [est * 1000.0]
        for _ in range(n - 1):
            t0 = time.perf_counter()
            engine.recognize(img)
            samples.append((time.perf_counter() - t0) * 1000.0)
        latency[lid] = {**latency_stats(samples), "reduced": n < N_WARM}

    result = {
        "config": args.config,
        "construction_ms": round(construction_ms, 1),
        "peak_rss_mib": round(peak_rss_mib(), 1),
        "accuracy": accuracy,
        "latency": latency,
    }
    if args.config == "meiki":
        (RESULTS / "meiki_lines.json").write_text(json.dumps(det_lines_out))

    out_path = RESULTS / (args.out or f"{args.config}.json")
    out_path.write_text(json.dumps(result, ensure_ascii=False, indent=1))
    print(f"{args.config}: {len(accuracy)} accuracy rows, rss {result['peak_rss_mib']} MiB")


if __name__ == "__main__":
    main()
