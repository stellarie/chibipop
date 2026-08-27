"""Drive the whole benchmark: sequential engine subprocesses (isolated RSS,
unloaded CPU for latency), then cold-start passes, then table generation.

    .venv/bin/python -m bench.run_all            # everything
    .venv/bin/python -m bench.run_all --only meiki ppocrv5
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys

from bench.common import CORPUS, RESULTS, ROOT

CONFIGS = ["meiki", "ppocrv5", "tess-fast", "tess-best", "manga-greedy", "manga-beam"]
COLD_IDS = {
    "meiki": ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"],
    "ppocrv5": ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"],
    "tess-fast": ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"],
    "tess-best": ["j1_1x", "j1_2x", "vert_1x", "vert_2x", "smoke_1x", "smoke_2x"],
    "manga-greedy": ["j1_2x", "vert_2x"],
    "manga-beam": ["j1_2x", "vert_2x"],
}


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    print("+", " ".join(cmd), flush=True)
    return subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", nargs="*", default=None)
    ap.add_argument("--skip-cold", action="store_true")
    args = ap.parse_args()
    configs = args.only or CONFIGS

    py = sys.executable
    if not (CORPUS / "manifest.json").exists():
        run([py, "-m", "bench.gen_corpus"])

    failures: dict[str, str] = {}
    for cfg in configs:
        try:
            p = run([py, "-m", "bench.run_one", "--config", cfg])
            print(p.stdout.strip())
        except subprocess.CalledProcessError as e:
            failures[cfg] = (e.stderr or "")[-2000:]
            print(f"FAILED {cfg}:\n{failures[cfg]}", file=sys.stderr)

    if not args.skip_cold:
        colds: dict[str, dict] = {}
        for cfg in configs:
            if cfg in failures:
                continue
            for cid in COLD_IDS[cfg]:
                try:
                    p = run([py, "-m", "bench.run_one", "--config", cfg, "--cold", cid])
                    rec = json.loads(p.stdout.strip().splitlines()[-1])
                    colds.setdefault(cfg, {})[cid] = rec
                except subprocess.CalledProcessError as e:
                    failures[f"{cfg}:cold:{cid}"] = (e.stderr or "")[-500:]
        (RESULTS / "cold.json").write_text(json.dumps(colds, indent=1))

    if failures:
        (RESULTS / "failures.json").write_text(json.dumps(failures, indent=1))
        print(f"{len(failures)} failure(s) recorded in results/failures.json")

    run([py, "-m", "bench.report"])
    print("done; tables in results/tables.md")


if __name__ == "__main__":
    main()
