from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

SCRIPT = Path(__file__).resolve().parents[1] / "ocr_resources.py"
spec = importlib.util.spec_from_file_location("ocr_resources", SCRIPT)
ocr_resources = importlib.util.module_from_spec(spec)
sys.modules["ocr_resources"] = ocr_resources
assert spec.loader is not None
spec.loader.exec_module(ocr_resources)


def record(phase: str, role: str, working: float, private: float, cpu: float,
           threads: int, handles: int) -> dict:
    """One complete record. Byte and MiB figures move together on purpose."""
    return {
        "timestamp": "2026-09-20T00:00:00",
        "phase": phase,
        "role": role,
        "pid": 1,
        "parent_pid": 0,
        "process_name": "chibipop",
        "executable_path": None,
        "working_set_bytes": int(working * 1024 * 1024),
        "working_set_mib": working,
        "private_bytes": int(private * 1024 * 1024),
        "private_mib": private,
        "cpu_seconds": cpu,
        "cpu_percent_one_core": None,
        "cpu_percent": None,
        "threads": threads,
        "handles": handles,
    }


class PhaseSpecTests(unittest.TestCase):
    def test_parses_a_label_and_a_duration(self) -> None:
        plan = ocr_resources.parse_phase_spec(["idle=5"])
        self.assertEqual(len(plan), 1)
        self.assertEqual(plan[0].label, "idle")
        self.assertEqual(plan[0].seconds, 5)
        self.assertEqual(plan[0].path, ())

    def test_parses_several_phases_in_order(self) -> None:
        plan = ocr_resources.parse_phase_spec(["idle=5", "hover=10"])
        self.assertEqual([phase.label for phase in plan], ["idle", "hover"])
        self.assertEqual([phase.seconds for phase in plan], [5, 10])

    def test_parses_a_pointer_path(self) -> None:
        plan = ocr_resources.parse_phase_spec(["hover=10:0,0;10,10;20,20"])
        self.assertEqual(plan[0].path, ((0, 0), (10, 10), (20, 20)))

    def test_ignores_a_trailing_path_separator(self) -> None:
        plan = ocr_resources.parse_phase_spec(["hover=10:0,0;"])
        self.assertEqual(plan[0].path, ((0, 0),))

    def test_rejects_a_missing_duration(self) -> None:
        with self.assertRaises(ValueError) as raised:
            ocr_resources.parse_phase_spec(["idle"])
        self.assertIn("idle", str(raised.exception))

    def test_rejects_a_zero_duration(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec(["idle=0"])

    def test_rejects_a_non_numeric_duration(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec(["idle=abc"])

    def test_rejects_an_empty_label(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec(["=5"])

    def test_rejects_a_duplicate_label(self) -> None:
        with self.assertRaises(ValueError) as raised:
            ocr_resources.parse_phase_spec(["idle=5", "idle=5"])
        self.assertIn("idle", str(raised.exception))

    def test_rejects_a_malformed_point(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec(["hover=5:10"])

    def test_rejects_a_non_numeric_coordinate(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec(["hover=5:a,1"])

    def test_rejects_an_empty_plan(self) -> None:
        with self.assertRaises(ValueError):
            ocr_resources.parse_phase_spec([])


class PhaseSummaryTests(unittest.TestCase):
    def test_reports_one_row_per_phase(self) -> None:
        summary = ocr_resources.summarize_phases([
            record("idle", "total", 30.0, 20.0, 1.0, 12, 300),
            record("hover", "total", 90.0, 70.0, 9.0, 18, 420),
        ])
        self.assertEqual([row["phase"] for row in summary], ["idle", "hover"])

    def test_reports_peak_and_last_values(self) -> None:
        summary = ocr_resources.summarize_phases([
            record("hover", "total", 80.0, 60.0, 7.0, 16, 400),
            record("hover", "total", 90.0, 55.0, 9.0, 18, 380),
        ])
        row = summary[0]
        self.assertEqual(row["sample_count"], 2)
        self.assertEqual(row["peak_working_set_mib"], 90.0)
        self.assertEqual(row["last_working_set_mib"], 90.0)
        self.assertEqual(row["peak_private_mib"], 60.0)
        self.assertEqual(row["last_private_mib"], 55.0)
        self.assertEqual(row["cpu_seconds_delta"], 2.0)
        self.assertEqual(row["peak_threads"], 18)
        self.assertEqual(row["peak_handles"], 400)

    def test_ignores_parent_rows(self) -> None:
        summary = ocr_resources.summarize_phases([
            record("idle", "parent", 10.0, 8.0, 0.5, 4, 100),
            record("idle", "total", 30.0, 20.0, 1.0, 12, 300),
        ])
        self.assertEqual(summary[0]["peak_working_set_mib"], 30.0)

    def test_skips_a_phase_without_a_total_row(self) -> None:
        summary = ocr_resources.summarize_phases([
            record("idle", "parent", 10.0, 8.0, 0.5, 4, 100),
        ])
        self.assertEqual(summary, [])

    def test_reports_an_empty_summary(self) -> None:
        self.assertEqual(ocr_resources.summarize_phases([]), [])

    def test_a_single_sample_has_a_zero_delta(self) -> None:
        summary = ocr_resources.summarize_phases([
            record("idle", "total", 12.0, 9.0, 2.0, 3, 30),
        ])
        self.assertEqual(summary[0]["cpu_seconds_delta"], 0.0)


class TreeSummaryTests(unittest.TestCase):
    def test_sums_one_sample_across_the_tree(self) -> None:
        rows = [
            record("idle", "parent", 10.0, 8.0, 0.5, 4, 100),
            record("idle", "descendant", 20.0, 12.0, 0.5, 8, 200),
        ]
        total = ocr_resources.tree_total(rows)
        self.assertEqual(total["role"], "total")
        self.assertEqual(total["working_set_mib"], 30.0)
        self.assertEqual(total["working_set_bytes"], 30 * 1024 * 1024)
        self.assertEqual(total["private_mib"], 20.0)
        self.assertEqual(total["cpu_seconds"], 1.0)
        self.assertEqual(total["threads"], 12)
        self.assertEqual(total["handles"], 300)

    def test_sums_an_empty_sample_to_zero(self) -> None:
        total = ocr_resources.tree_total([])
        self.assertEqual(total["working_set_mib"], 0.0)
        self.assertEqual(total["threads"], 0)


class ChildIdsTests(unittest.TestCase):
    def test_returns_the_root_and_its_descendants(self) -> None:
        table = [
            {"process_id": 1, "parent_process_id": 0},
            {"process_id": 2, "parent_process_id": 1},
            {"process_id": 3, "parent_process_id": 2},
            {"process_id": 4, "parent_process_id": 99},
        ]
        self.assertEqual(ocr_resources.child_ids(1, table), [1, 2, 3])

    def test_returns_only_the_root_without_children(self) -> None:
        table = [{"process_id": 7, "parent_process_id": 0}]
        self.assertEqual(ocr_resources.child_ids(7, table), [7])

    def test_returns_nothing_for_an_absent_root(self) -> None:
        self.assertEqual(ocr_resources.child_ids(5, []), [])


class CliTests(unittest.TestCase):
    def test_help_exits_cleanly(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            ocr_resources.main(["--help"])
        self.assertEqual(raised.exception.code, 0)

    def test_rejects_a_bad_phase_spec_with_a_usage_error(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                ocr_resources.main(["--file", "chibipop", "--phase", "idle"])
        self.assertEqual(raised.exception.code, 2)

    def test_requires_a_file(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                ocr_resources.main(["--phase", "idle=1"])
        self.assertEqual(raised.exception.code, 2)

    def test_dry_run_prints_the_plan_and_samples_nothing(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ocr_resources.main([
                "--file", "chibipop",
                "--phase", "idle=5",
                "--phase", "hover=10:1,2;3,4",
                "--dry-run",
            ])
        self.assertEqual(code, 0)
        plan = json.loads(out.getvalue())
        self.assertEqual(plan["file"], "chibipop")
        self.assertEqual([phase["label"] for phase in plan["phases"]], ["idle", "hover"])
        self.assertEqual(plan["phases"][1]["path"], [{"x": 1, "y": 2}, {"x": 3, "y": 4}])

    def test_dry_run_defaults_to_one_labelled_phase(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ocr_resources.main(["--file", "chibipop", "--dry-run"])
        self.assertEqual(code, 0)
        plan = json.loads(out.getvalue())
        self.assertEqual([phase["label"] for phase in plan["phases"]], ["ocr"])
        self.assertEqual(plan["phases"][0]["seconds"], 60)

    def test_writes_a_report_with_a_phase_summary(self) -> None:
        rows = [
            record("idle", "total", 30.0, 20.0, 1.0, 12, 300),
            record("hover", "total", 90.0, 70.0, 9.0, 18, 420),
        ]
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "perf.json"
            code = ocr_resources.write_report(
                out,
                file="chibipop",
                arguments=["run"],
                phases=ocr_resources.parse_phase_spec(["idle=1", "hover=1"]),
                records=rows,
                stopped=[11],
                remaining=[],
            )
            self.assertEqual(code, 0)
            report = json.loads(out.read_text(encoding="utf-8"))
        self.assertEqual(report["schema"], "chibipop-ocr-resources/v1")
        self.assertEqual(report["cleanup_stopped_process_ids"], [11])
        self.assertEqual(len(report["records"]), 2)
        self.assertEqual([row["phase"] for row in report["phase_summary"]], ["idle", "hover"])

    def test_report_flags_a_surviving_process(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "perf.json"
            code = ocr_resources.write_report(
                out,
                file="chibipop",
                arguments=[],
                phases=ocr_resources.parse_phase_spec(["idle=1"]),
                records=[],
                stopped=[],
                remaining=[42],
            )
            report = json.loads(out.read_text(encoding="utf-8"))
        self.assertEqual(code, 1)
        self.assertEqual(report["cleanup_remaining_process_ids"], [42])


class WindowsBackendTests(unittest.TestCase):
    """The Windows backend and the Python sampler share one contract."""

    def test_the_backend_script_sits_beside_the_sampler(self) -> None:
        sampler = ocr_resources.WindowsSampler("chibipop", [], 100)
        self.assertTrue(sampler.script_path().is_file(), sampler.script_path())

    @unittest.skipUnless(
        sys.platform == "win32" and shutil.which("pwsh"),
        "the process-tree backend needs Windows and pwsh",
    )
    def test_the_backend_emits_records_and_one_total_row_per_phase(self) -> None:
        # A silent backend returned exit 0 and zero records once: the emitting
        # function also returned a value, so PowerShell handed every record to
        # the caller's variable instead of to stdout. This test fails on that.
        sampler = ocr_resources.WindowsSampler(
            shutil.which("pwsh") or "pwsh",
            ["-NoProfile", "-Command", "Start-Sleep -Seconds 20"],
            200,
        )
        records, stopped, remaining = sampler.sample(
            ocr_resources.parse_phase_spec(["idle=2"])
        )
        self.assertEqual([], remaining)
        self.assertTrue(stopped, "the child was never reaped")
        self.assertTrue(records, "the backend emitted nothing on stdout")
        for row in records:
            self.assertEqual(sorted(ocr_resources.RECORD_KEYS), sorted(row))
        totals = [row for row in records if row["role"] == "total"]
        self.assertTrue(totals, "no phase summary row")
        self.assertTrue(all(row["phase"] == "idle" for row in totals))
        self.assertGreater(totals[-1]["working_set_bytes"], 0)

    def test_a_host_without_a_sampler_refuses_instead_of_guessing(self) -> None:
        if sys.platform == "win32":
            self.skipTest("this host has the Windows sampler")
        with self.assertRaises(ValueError):
            ocr_resources.sampler_for("chibipop", [], 100)


if __name__ == "__main__":
    unittest.main()
