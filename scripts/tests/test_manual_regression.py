import importlib.util
import contextlib
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "manual_regression.py"
spec = importlib.util.spec_from_file_location("manual_regression", SCRIPT)
manual_regression = importlib.util.module_from_spec(spec)
sys.modules["manual_regression"] = manual_regression
assert spec.loader is not None
spec.loader.exec_module(manual_regression)


def numbered(prefix: str, first: int, last: int) -> set[str]:
    return {f"{prefix}.{index}" for index in range(first, last + 1)}


class ManualRegressionTests(unittest.TestCase):
    def test_suite_includes_documented_items(self) -> None:
        ids = {check.ident for check in manual_regression.build_checks()}
        all_ids = [check.ident for check in manual_regression.build_checks()]
        self.assertEqual(len(all_ids), len(ids))
        required = (
            numbered("0", 1, 5)
            | numbered("1", 1, 41)
            | {"1.8.1"}
            | {"1.7a"}
            | numbered("1.11", 1, 3)
            | numbered("1.14", 1, 5)
            | numbered("1.15", 1, 5)
            | numbered("1.16", 1, 7)
            | numbered("1.17", 1, 13)
            | numbered("1.18", 1, 15)
            | numbered("1.19", 1, 6)
            | numbered("1.20", 1, 2)
            | numbered("1.21", 1, 4)
            | numbered("1.22", 1, 7)
            | numbered("1.23", 1, 3)
            | numbered("1.25", 1, 5)
            | numbered("1.26", 1, 7)
            | numbered("1.27", 1, 5)
            | numbered("1.28", 1, 7)
            | numbered("1.29", 1, 4)
            | numbered("1.30", 1, 16)
            | numbered("1.31", 1, 4)
            | numbered("1.32", 1, 3)
            | numbered("1.33", 1, 5)
            | numbered("1.34", 1, 5)
            | numbered("1.35", 1, 4)
            | numbered("1.36", 1, 4)
            | numbered("1.37", 1, 2)
            | numbered("1.38", 1, 1)
            | numbered("1.39", 1, 3)
            | numbered("1.40", 1, 4)
            | numbered("2", 1, 14)
            | {"2.11a", "2.11b", "2.11c", "2.11d", "2.11e", "2.11f"}
            | {"2.14a", "2.14b", "2.14c", "2.14d", "2.14e", "2.14f"}
        )
        self.assertSetEqual(ids, required)

    def test_status_set_matches_runner_contract(self) -> None:
        statuses = manual_regression.summarize(
            [
                manual_regression.Result("a", "0", "a", "auto", "PASS"),
                manual_regression.Result("b", "0", "b", "auto", "FAIL"),
                manual_regression.Result("c", "0", "c", "auto", "SKIP"),
                manual_regression.Result("d", "0", "d", "auto", "XFAIL"),
                manual_regression.Result("e", "0", "e", "auto", "MANUAL"),
            ]
        )
        self.assertEqual(
            set(statuses),
            {"PASS", "FAIL", "SKIP", "XFAIL", "MANUAL"},
        )

    def test_selectors_match_exact_and_children(self) -> None:
        self.assertTrue(manual_regression.matches_selector("1.18.15", "1.18"))
        self.assertTrue(manual_regression.matches_selector("1.7a", "1.7"))
        self.assertFalse(manual_regression.matches_selector("1.18", "1.1"))

    def test_known_gaps_are_marked(self) -> None:
        known = {
            check.ident
            for check in manual_regression.build_checks()
            if check.known_gap
        }
        self.assertEqual(
            known,
            {
                "1.6",
                "1.14.5",
                "1.27",
                "1.27.4",
                "1.27.5",
                "2.9",
                "2.11a",
            },
        )

    def test_clippy_commands_match_documented_gate(self) -> None:
        calls = []

        def fake_run_cmd(cmd, cwd, logs_dir, name, timeout=None):
            calls.append(cmd)
            output = "warning: this function has too many arguments\nwarning: `x` generated 1 warning\n"
            return 0, output, 0.0, Path("clippy.log")

        original = manual_regression.run_cmd
        manual_regression.run_cmd = fake_run_cmd
        try:
            args = type(
                "Args",
                (),
                {
                    "cargo": "cargo",
                    "repo_root": Path("."),
                    "expected_clippy_warnings": 1,
                },
            )()
            result = manual_regression.auto_clippy_accepted(
                manual_regression.Check("0.2", "0", "clippy", "auto", "", ""),
                args,
                Path("."),
            )
        finally:
            manual_regression.run_cmd = original
        self.assertEqual(result.status, "PASS")
        self.assertEqual(
            calls[0],
            [
                "cargo",
                "clippy",
                "--workspace",
                "--color",
                "never",
                "--all-targets",
                "--all-features",
            ],
        )

    def test_suppressed_clippy_rejects_warnings_and_errors(self) -> None:
        def fake_run_cmd(cmd, cwd, logs_dir, name, timeout=None):
            output = "warning: allowed lint\nerror: could not compile `x`\n"
            return 0, output, 0.0, Path("clippy.log")

        original = manual_regression.run_cmd
        manual_regression.run_cmd = fake_run_cmd
        try:
            args = type(
                "Args",
                (),
                {
                    "cargo": "cargo",
                    "repo_root": Path("."),
                    "expected_other_clippy": 0,
                },
            )()
            result = manual_regression.auto_clippy_suppressed(
                manual_regression.Check("0.3", "0", "clippy", "auto", "", ""),
                args,
                Path("."),
            )
        finally:
            manual_regression.run_cmd = original
        self.assertEqual(result.status, "FAIL")

    def test_clippy_process_failures_never_pass_from_counts_alone(self) -> None:
        original = manual_regression.run_cmd
        args = type("Args", (), {"cargo": "cargo", "repo_root": Path("."),
                                 "expected_clippy_warnings": 1, "expected_other_clippy": 0})()
        try:
            for handler, output in [
                (manual_regression.auto_clippy_accepted, "warning: accepted finding\n"),
                (manual_regression.auto_clippy_suppressed, ""),
            ]:
                manual_regression.run_cmd = lambda *a, **kw: (1, output, 0.0, Path("clippy.log"))
                result = handler(manual_regression.Check("0", "0", "clippy", "auto", "", ""), args, Path("."))
                self.assertEqual(result.status, "FAIL")
        finally:
            manual_regression.run_cmd = original

    def test_relative_target_directory_resolves_under_repo_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            install = root / "install"
            install.mkdir()
            target = manual_regression.parse_target("main=install", root)
            self.assertEqual(target.exe, install / "chibipop.exe")

    def test_timeout_is_logged_as_nonzero_result(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            code, output, _, log = manual_regression.run_cmd(
                [
                    sys.executable,
                    "-c",
                    "import time; time.sleep(2)",
                ],
                root,
                root,
                "timeout",
                timeout=1,
            )
            self.assertEqual(code, 124)
            self.assertIn("timed out", output)
            self.assertTrue(log.exists())

    def test_authorization_gates_use_prefixes_and_metadata(self) -> None:
        checks = {check.ident: check for check in manual_regression.build_checks()}
        self.assertTrue(manual_regression.requires_anki_write(checks["1.11"]))
        self.assertTrue(manual_regression.requires_anki_write(checks["1.30"]))
        self.assertTrue(manual_regression.requires_anki_write(checks["1.30.4"]))
        self.assertTrue(manual_regression.requires_anki_write(checks["1.30.9"]))
        self.assertFalse(manual_regression.requires_anki_write(checks["1.30.5"]))
        self.assertTrue(manual_regression.requires_anki_write(checks["1.22.6"]))
        self.assertTrue(manual_regression.requires_dictionary_mutation(checks["1.19.6"]))
        self.assertTrue(manual_regression.requires_config_write(checks["1.14.2"]))
        self.assertFalse(manual_regression.requires_display_change(checks["1.26"]))
        self.assertTrue(manual_regression.requires_display_change(checks["1.26.7"]))

    def test_source_does_not_embed_local_machine_paths(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        slash_user = "/c/" + "Users" + "/"
        win_user = "Users" + "\\" + "St" + "ella"
        nightly_name = "chibipop-" + "nightly"
        banned = [chr(67) + ":" + "\\", slash_user, win_user, nightly_name]
        self.assertFalse(any(item in source for item in banned))

    def test_new_cases_and_all_document_references_resolve(self) -> None:
        doc = (SCRIPT.parents[1] / "docs/REGRESSION.md").read_text(encoding="utf-8")
        anchors = set(re.findall(r'<a id="([^"]+)"></a>', doc))
        for heading in re.findall(r"^#{1,6}\s+(.+)$", doc, re.MULTILINE):
            anchors.add(re.sub(r"[^\w\s-]", "", heading.lower()).replace(" ", "-"))
        documented_rows = re.findall(r"^\| (\d+(?:\.\d+)+) \|", doc, re.MULTILINE)
        documented = set(documented_rows)
        self.assertEqual(len(documented_rows), len(documented))
        checks = manual_regression.build_checks()
        for check in checks:
            self.assertTrue(check.prompt.strip(), check.ident)
            path, anchor = check.doc_ref.split("#", 1)
            self.assertEqual(path, "docs/REGRESSION.md")
            self.assertIn(anchor, anchors, check.ident)
            if check.ident == "1.8.1" or any(manual_regression.matches_selector(check.ident, f"1.{n}") for n in range(31, 42)):
                self.assertIn(check.ident, documented)
        for index in range(11, 17):
            self.assertIn(f"1.30.{index}", documented)
        self.assertTrue(documented.issubset({check.ident for check in checks}))

    def test_new_effects_and_partial_automation_are_explicit(self) -> None:
        checks = {check.ident: check for check in manual_regression.build_checks()}
        for ident in ["1.4", "1.8", "1.26"]:
            self.assertIsNone(checks[ident].auto)
            self.assertEqual(checks[ident].mode, "interactive")
        self.assertEqual(checks["1.8.1"].auto, "resources")
        self.assertEqual(checks["1.26.1"].mode, "auto")
        for check in checks.values():
            if check.effects:
                self.assertTrue(check.destructive, check.ident)
        for ident in ["1.17.5", "1.18.9", "1.20", "2.12", "1.33.1", "1.37.2"]:
            self.assertTrue(manual_regression.requires_config_write(checks[ident]), ident)
        self.assertTrue(manual_regression.requires_display_change(checks["1.34.4"]))
        self.assertFalse(manual_regression.requires_display_change(checks["1.38"]))
        for effect, gate in [("config", manual_regression.requires_config_write),
                             ("dictionary", manual_regression.requires_dictionary_mutation),
                             ("anki", manual_regression.requires_anki_write),
                             ("display", manual_regression.requires_display_change)]:
            check = manual_regression.Check("new", "1", "fixture", "interactive", "", "", effects=(effect,))
            self.assertTrue(gate(check), effect)

    def test_list_exposes_case_references_and_effects_without_execution(self) -> None:
        result = subprocess.run([sys.executable, str(SCRIPT), "--list", "--only", "1.36"],
                                capture_output=True, text=True, check=True)
        self.assertIn("1.36.4", result.stdout)
        self.assertIn("ref=docs/REGRESSION.md#case-1-36", result.stdout)
        self.assertIn("effects=clipboard", result.stdout)
        self.assertIn("destructive=true", result.stdout)
        self.assertNotIn("wrote report:", result.stdout)

    def test_report_keeps_selected_case_metadata_and_permissions(self) -> None:
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(sys, "argv", [str(SCRIPT), "--only", "1.37", "--non-interactive"]):
                args = manual_regression.parse_args()
            args.repo_root = Path(tmp)
            args.report = Path(tmp) / "report.json"
            check = next(c for c in manual_regression.build_checks() if c.ident == "1.37")
            result = manual_regression.manual_check(check, "not executed")
            with contextlib.redirect_stdout(io.StringIO()):
                manual_regression.write_report(args, [], [result], {})
            report = json.loads(args.report.read_text(encoding="utf-8"))
            self.assertEqual({c["ident"] for c in report["checks"]}, {"1.37", "1.37.1", "1.37.2"})
            self.assertIn("doc_ref", report["checks"][0])
            self.assertIn("effects", report["checks"][0])
            self.assertFalse(report["args"]["interactive"])
            self.assertFalse(report["args"]["allow_config_write"])
            self.assertFalse(report["args"]["allow_anki_write"])
            self.assertEqual(report["summary"]["MANUAL"], 1)
            self.assertEqual(report["summary"]["PASS"], 0)


if __name__ == "__main__":
    unittest.main()
