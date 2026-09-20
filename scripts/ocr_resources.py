#!/usr/bin/env python3
"""Sample the process tree of one chibipop session, phase by phase.

The phase planner and the report are pure Python so they run on every
platform. Only the process-tree sampling and the pointer path are
platform-native, and both live behind `sampler_for`.

`scripts/measure_ocr_resources.ps1` remains the original single-phase
sampler. This module is the phased replacement: it accepts an ordered plan
such as `idle=20` then `hover=30:1200,400;1400,400`, drives the pointer
along each path, and reports one summary row per phase.

Exit codes are the repository tool convention: 0 success, 1 the run
failed or a process survived cleanup, 2 bad arguments.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Sequence

SCHEMA = "chibipop-ocr-resources/v1"
DEFAULT_SECONDS = 60
DEFAULT_SAMPLE_MILLISECONDS = 100

# The keys every record carries. The sampler fills them; the summary and the
# report read them, so a platform sampler only has to produce this shape.
RECORD_KEYS = (
    "timestamp",
    "phase",
    "role",
    "pid",
    "parent_pid",
    "process_name",
    "executable_path",
    "working_set_bytes",
    "working_set_mib",
    "private_bytes",
    "private_mib",
    "cpu_seconds",
    "cpu_percent_one_core",
    "cpu_percent",
    "threads",
    "handles",
)


@dataclass(frozen=True)
class Phase:
    """One ordered measurement phase."""

    label: str
    seconds: int
    path: tuple[tuple[int, int], ...] = ()


def parse_phase_spec(specs: Sequence[str]) -> list[Phase]:
    """Parse `label=seconds[:x,y;x,y]` entries into an ordered plan."""
    if not specs:
        raise ValueError("at least one phase is required")
    phases: list[Phase] = []
    seen: set[str] = set()
    for entry in specs:
        label, separator, rest = entry.partition("=")
        label = label.strip()
        if not separator or not label:
            raise ValueError(f"phase '{entry}' must read label=seconds or label=seconds:path")
        if label in seen:
            raise ValueError(f"phase '{label}' is declared twice")
        seen.add(label)
        seconds_text, _, path_text = rest.partition(":")
        try:
            seconds = int(seconds_text.strip())
        except ValueError:
            raise ValueError(
                f"phase '{label}' has a non-numeric duration '{seconds_text.strip()}'"
            ) from None
        if seconds < 1:
            raise ValueError(f"phase '{label}' needs a duration of at least one second")
        phases.append(Phase(label=label, seconds=seconds, path=parse_pointer_path(label, path_text)))
    return phases


def parse_pointer_path(label: str, text: str) -> tuple[tuple[int, int], ...]:
    """Parse a `x,y;x,y` pointer path. An empty path keeps the pointer still."""
    points: list[tuple[int, int]] = []
    for pair in text.split(";"):
        pair = pair.strip()
        if not pair:
            continue
        parts = pair.split(",")
        if len(parts) != 2:
            raise ValueError(f"phase '{label}' has a malformed point '{pair}'")
        try:
            points.append((int(parts[0].strip()), int(parts[1].strip())))
        except ValueError:
            raise ValueError(f"phase '{label}' has a non-numeric point '{pair}'") from None
    return tuple(points)


def child_ids(root: int, table: Iterable[dict]) -> list[int]:
    """Return the root and every descendant, parents before children."""
    parents: dict[int, list[int]] = {}
    ids: set[int] = set()
    for row in table:
        process_id = int(row["process_id"])
        ids.add(process_id)
        parents.setdefault(int(row["parent_process_id"]), []).append(process_id)
    if root not in ids:
        return []
    found = [root]
    pending = [root]
    while pending:
        for child in parents.get(pending.pop(0), ()):
            if child not in found:
                found.append(child)
                pending.append(child)
    return found


def tree_total(rows: Sequence[dict]) -> dict:
    """Sum one sample's rows into the process-tree total row."""
    return {
        "timestamp": rows[-1].get("timestamp", "") if rows else "",
        "phase": rows[0].get("phase", "") if rows else "",
        "role": "total",
        "pid": 0,
        "parent_pid": 0,
        "process_name": "process-tree-total",
        "executable_path": None,
        "working_set_bytes": sum(int(row["working_set_bytes"]) for row in rows),
        "working_set_mib": round(sum(float(row["working_set_mib"]) for row in rows), 3),
        "private_bytes": sum(int(row["private_bytes"]) for row in rows),
        "private_mib": round(sum(float(row["private_mib"]) for row in rows), 3),
        "cpu_seconds": round(sum(float(row["cpu_seconds"]) for row in rows), 6),
        "cpu_percent_one_core": None,
        "cpu_percent": None,
        "threads": sum(int(row["threads"]) for row in rows),
        "handles": sum(int(row["handles"]) for row in rows),
    }


def summarize_phases(records: Sequence[dict]) -> list[dict]:
    """Summarize one row per phase from that phase's tree-total samples."""
    order: list[str] = []
    by_phase: dict[str, list[dict]] = {}
    for row in records:
        if row.get("role") != "total":
            continue
        phase = row["phase"]
        if phase not in by_phase:
            by_phase[phase] = []
            order.append(phase)
        by_phase[phase].append(row)
    summary = []
    for phase in order:
        rows = by_phase[phase]
        summary.append({
            "phase": phase,
            "sample_count": len(rows),
            "peak_working_set_mib": round(max(float(row["working_set_mib"]) for row in rows), 3),
            "last_working_set_mib": round(float(rows[-1]["working_set_mib"]), 3),
            "peak_private_mib": round(max(float(row["private_mib"]) for row in rows), 3),
            "last_private_mib": round(float(rows[-1]["private_mib"]), 3),
            "cpu_seconds_delta": round(
                float(rows[-1]["cpu_seconds"]) - float(rows[0]["cpu_seconds"]), 6
            ),
            "last_cpu_seconds": round(float(rows[-1]["cpu_seconds"]), 6),
            "peak_threads": max(int(row["threads"]) for row in rows),
            "peak_handles": max(int(row["handles"]) for row in rows),
        })
    return summary


def plan_payload(phases: Sequence[Phase]) -> list[dict]:
    """Render the resolved plan for the report and the dry run."""
    return [
        {
            "label": phase.label,
            "seconds": phase.seconds,
            "path": [{"x": x, "y": y} for x, y in phase.path],
        }
        for phase in phases
    ]


def write_report(
    path: Path,
    *,
    file: str,
    arguments: Sequence[str],
    phases: Sequence[Phase],
    records: Sequence[dict],
    stopped: Sequence[int],
    remaining: Sequence[int],
    root_pid: int | None = None,
) -> int:
    """Write one report. Return 0 when cleanup left nothing behind."""
    report = {
        "schema": SCHEMA,
        "sampled_until": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "root_pid": root_pid,
        "command": {"file": file, "arguments": list(arguments)},
        "phases": plan_payload(phases),
        "phase_summary": summarize_phases(records),
        "cleanup_stopped_process_ids": list(stopped),
        "cleanup_remaining_process_ids": list(remaining),
        "records": list(records),
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"wrote report: {path}")
    for row in report["phase_summary"]:
        print(
            f"{row['phase']:12} samples={row['sample_count']:4} "
            f"peak_ws={row['peak_working_set_mib']:9.3f} MiB "
            f"peak_private={row['peak_private_mib']:9.3f} MiB "
            f"cpu_delta={row['cpu_seconds_delta']:8.3f} s "
            f"threads={row['peak_threads']:4} handles={row['peak_handles']:6}"
        )
    return 1 if remaining else 0


class Sampler:
    """Platform sampler seam. One implementation per platform."""

    def __init__(self, file: str, arguments: Sequence[str], sample_ms: int) -> None:
        self.file = file
        self.arguments = list(arguments)
        self.sample_ms = sample_ms

    def sample(self, phases: Sequence[Phase]) -> tuple[list[dict], list[int], list[int]]:
        raise NotImplementedError

    def close(self) -> None:
        pass


class WindowsSampler(Sampler):
    """Drive the child, then ask PowerShell once per phase for the tree."""

    def __init__(self, file: str, arguments: Sequence[str], sample_ms: int) -> None:
        super().__init__(file, arguments, sample_ms)
        self.child: subprocess.Popen | None = None

    def script_path(self) -> Path:
        return Path(__file__).resolve().parent / "ocr_resources_windows.ps1"

    def sample(self, phases: Sequence[Phase]) -> tuple[list[dict], list[int], list[int]]:
        with tempfile.TemporaryDirectory(prefix="chibipop-perf-") as temporary:
            plan_path = Path(temporary) / "plan.json"
            plan_path.write_text(
                json.dumps({"phases": plan_payload(phases), "sample_ms": self.sample_ms}),
                encoding="utf-8",
            )
            self.child = subprocess.Popen(
                [self.file, *self.arguments],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                stdin=subprocess.DEVNULL,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            completed = subprocess.run(
                [
                    "pwsh", "-NoProfile", "-NonInteractive", "-File", str(self.script_path()),
                    "-RootPid", str(self.child.pid), "-PlanPath", str(plan_path),
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=sum(phase.seconds for phase in phases) + 120,
            )
        if completed.returncode != 0:
            raise RuntimeError(f"sampler failed: {completed.stderr.strip()}")
        records: list[dict] = []
        for line in completed.stdout.splitlines():
            line = line.strip()
            if line:
                records.append(json.loads(line))
        stopped, remaining = self.stop()
        return records, stopped, remaining

    def stop(self) -> tuple[list[int], list[int]]:
        if self.child is None:
            return [], []
        remaining: list[int] = []
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=10)
        if self.child.poll() is None:
            remaining.append(self.child.pid)
        stopped = [] if remaining else [self.child.pid]
        return stopped, remaining


def sampler_for(file: str, arguments: Sequence[str], sample_ms: int) -> Sampler:
    """Select the platform sampler, or refuse on a host that has none."""
    if os.name == "nt":
        return WindowsSampler(file, arguments, sample_ms)
    raise ValueError("resource sampling needs Windows; the phase planner and report are portable")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        prog="ocr_resources.py",
        description="Sample one chibipop process tree phase by phase.",
    )
    result.add_argument("--file", required=True, help="chibipop executable or install directory.")
    result.add_argument("--arguments", default="", help="Comma-separated arguments for the child.")
    result.add_argument("--phase", action="append", default=[], metavar="LABEL=SECONDS[:PATH]",
                        help="Ordered phase. Repeat for each phase. Defaults to one 'ocr' phase.")
    result.add_argument("--sample-milliseconds", type=int, default=DEFAULT_SAMPLE_MILLISECONDS)
    result.add_argument("--output", type=Path, help="JSON report path.")
    result.add_argument("--dry-run", action="store_true", help="Print the resolved plan and exit.")
    return result


def resolved_phases(args: argparse.Namespace) -> list[Phase]:
    if args.phase:
        return parse_phase_spec(args.phase)
    return [Phase(label="ocr", seconds=DEFAULT_SECONDS)]


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        phases = resolved_phases(args)
    except ValueError as error:
        parser().error(str(error))
    if args.sample_milliseconds < 10:
        parser().error("--sample-milliseconds must be at least 10")
    payload = {
        "file": args.file,
        "arguments": [part for part in args.arguments.split(",") if part],
        "sample_milliseconds": args.sample_milliseconds,
        "phases": plan_payload(phases),
    }
    if args.dry_run:
        print(json.dumps(payload, indent=2))
        return 0
    try:
        sampler = sampler_for(args.file, payload["arguments"], args.sample_milliseconds)
    except ValueError as error:
        print(f"ocr_resources: {error}", file=sys.stderr)
        return 2
    output = args.output or Path(f"ocr-resources-{time.strftime('%Y%m%d-%H%M%S')}.json")
    try:
        records, stopped, remaining = sampler.sample(phases)
    finally:
        sampler.close()
    return write_report(
        output,
        file=args.file,
        arguments=payload["arguments"],
        phases=phases,
        records=records,
        stopped=stopped,
        remaining=remaining,
    )


if __name__ == "__main__":
    raise SystemExit(main())
