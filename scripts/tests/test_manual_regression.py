import importlib.util
from pathlib import Path
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
            | numbered("1", 1, 30)
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
            | numbered("1.30", 1, 10)
            | numbered("2", 1, 14)
            | {"2.11a", "2.11b", "2.11c", "2.11d", "2.11e"}
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
                "1.27.2",
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
            output = "error: this function has too many arguments\nerror: could not compile `x`\n"
            return 101, output, 0.0, Path("clippy.log")

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
                "--",
                "-D",
                "warnings",
            ],
        )

    def test_suppressed_clippy_counts_error_lines_only(self) -> None:
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
        self.assertEqual(result.status, "PASS")

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


if __name__ == "__main__":
    unittest.main()
