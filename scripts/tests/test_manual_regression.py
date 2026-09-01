import importlib.util
import json
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
            self.assertFalse(target.disposable)

    def test_seed_test_install_copies_release_package(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            files = [
                "target/release/chibipop.exe",
                "data/deconjugator.json",
                "README.md",
                "LICENSE",
                "plugins/meikiocr/plugin.toml",
                "plugins/meikiocr/adapter.py",
                "plugins/meikiocr/config.toml",
            ]
            for name in files:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(name, encoding="utf-8")

            def fake_run_cmd(cmd, cwd, logs_dir, name, timeout=None):
                log = logs_dir / "build.log"
                log.parent.mkdir(parents=True, exist_ok=True)
                log.write_text("built", encoding="utf-8")
                return 0, "built", 0.1, log

            original = manual_regression.run_cmd
            manual_regression.run_cmd = fake_run_cmd
            args = type(
                "Args",
                (),
                {
                    "cargo": "cargo",
                    "repo_root": root,
                    "test_install_dir": Path(".scratch/regression-test-install"),
                },
            )()
            try:
                target, result = manual_regression.seed_test_install(args, root / "logs")
            finally:
                manual_regression.run_cmd = original
                manual_regression.release_test_install_lock(args)

            self.assertEqual(result.status, "PASS")
            self.assertTrue(target.disposable)
            self.assertEqual(target.name, "test-install")
            self.assertTrue(target.exe.exists())
            self.assertTrue((target.root / manual_regression.TEST_INSTALL_MARKER).exists())
            self.assertTrue((target.root / "plugins" / "meikiocr" / "config.toml").exists())

    def test_seed_test_install_refuses_unmarked_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            install = root / ".scratch" / "regression-test-install"
            install.mkdir(parents=True)
            (install / "someone-elses-file.txt").write_text("do not remove", encoding="utf-8")

            def fake_run_cmd(cmd, cwd, logs_dir, name, timeout=None):
                log = logs_dir / "build.log"
                log.parent.mkdir(parents=True, exist_ok=True)
                log.write_text("built", encoding="utf-8")
                return 0, "built", 0.1, log

            original = manual_regression.run_cmd
            manual_regression.run_cmd = fake_run_cmd
            args = type(
                "Args",
                (),
                {
                    "cargo": "cargo",
                    "repo_root": root,
                    "test_install_dir": Path(".scratch/regression-test-install"),
                },
            )()
            try:
                with self.assertRaises(ValueError):
                    manual_regression.seed_test_install(args, root / "logs")
            finally:
                manual_regression.run_cmd = original
                manual_regression.release_test_install_lock(args)

            self.assertTrue((install / "someone-elses-file.txt").exists())

    def test_unsafe_test_install_paths_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                manual_regression.assert_safe_test_install_dir(Path("."), root)
            with self.assertRaises(ValueError):
                manual_regression.assert_safe_test_install_dir(Path("target"), root)
            with self.assertRaises(ValueError):
                manual_regression.assert_safe_test_install_dir(Path("target/release-copy"), root)
            with self.assertRaises(ValueError):
                manual_regression.assert_safe_test_install_dir(Path("docs/regression-install"), root)
            with self.assertRaises(ValueError):
                manual_regression.assert_safe_test_install_dir(Path(".scratch/../src/install"), root)

    def test_marker_must_match_schema_and_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            install = root / ".scratch" / "regression-test-install"
            install.mkdir(parents=True)
            marker = install / manual_regression.TEST_INSTALL_MARKER
            marker.write_text("{}", encoding="utf-8")
            self.assertFalse(manual_regression.marker_identifies_test_install(marker, install))
            marker.write_text(
                '{"schema":"chibipop-test-install/v1","root":"'
                + str(root / "other").replace("\\", "\\\\")
                + '"}',
                encoding="utf-8",
            )
            self.assertFalse(manual_regression.marker_identifies_test_install(marker, install))
            marker.write_text(
                '{"schema":"chibipop-test-install/v1","root":"'
                + str(install).replace("\\", "\\\\")
                + '"}',
                encoding="utf-8",
            )
            self.assertTrue(manual_regression.marker_identifies_test_install(marker, install))

    def test_cleanup_refuses_invalid_marker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            install = root / ".scratch" / "regression-test-install"
            install.mkdir(parents=True)
            (install / manual_regression.TEST_INSTALL_MARKER).write_text("{}", encoding="utf-8")
            args = type(
                "Args",
                (),
                {
                    "repo_root": root,
                    "test_install": True,
                    "keep_test_install": False,
                    "test_install_dir": Path(".scratch/regression-test-install"),
                },
            )()
            result = manual_regression.cleanup_test_install(args)
            self.assertIsNotNone(result)
            assert result is not None
            self.assertEqual(result.status, "FAIL")
            self.assertTrue(install.exists())

    def test_main_cleans_test_install_and_writes_report_after_auto_exception(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            install = root / ".scratch" / "regression-test-install"
            install.mkdir(parents=True)
            (install / "chibipop.exe").write_text("exe", encoding="utf-8")
            marker = install / manual_regression.TEST_INSTALL_MARKER
            marker.write_text(
                '{"schema":"chibipop-test-install/v1","root":"'
                + str(install).replace("\\", "\\\\")
                + '"}',
                encoding="utf-8",
            )
            args = type(
                "Args",
                (),
                {
                    "repo_root": root,
                    "cargo": "cargo",
                    "exe": None,
                    "secondary_exe": [],
                    "target": [],
                    "tier": "all",
                    "only": [],
                    "skip": [],
                    "list": False,
                    "artifacts_dir": root / "artifacts",
                    "report": root / "artifacts" / "report.json",
                    "logs_dir": root / "artifacts" / "logs",
                    "interactive": False,
                    "strict": False,
                    "allow_destructive": False,
                    "allow_config_write": False,
                    "allow_dictionary_mutation": False,
                    "allow_anki_write": False,
                    "allow_display_change": False,
                    "allow_real_target_destructive": False,
                    "keep_mutated_state": False,
                    "allow_plugin_fixtures": False,
                    "stop_target_strays": False,
                    "test_install": True,
                    "test_install_dir": Path(".scratch/regression-test-install"),
                    "keep_test_install": False,
                    "repeat_tests": 1,
                    "min_test_total": 0,
                    "expected_clippy_warnings": 1,
                    "expected_other_clippy": 0,
                    "allow_local_golden_failure": False,
                    "probe_point": [],
                    "region": "",
                    "ja_point": "",
                    "zh_simplified_point": "",
                    "zh_traditional_point": "",
                    "alnum_point": "",
                    "vertical_point": "",
                    "show_region_seconds": 1,
                    "open_fixtures": False,
                    "browser_command": [],
                    "browser_cmd_template": None,
                    "corpus": None,
                    "scroll_fixture": None,
                    "plugin_image": None,
                    "dictionary_archive": [],
                    "term_archive": [],
                    "frequency_archive": [],
                    "corrupt_archive": None,
                    "primary_language": "",
                    "secondary_language": [],
                    "anki_deck": "",
                },
            )()

            def fake_parse_args():
                return args

            def fake_seed_test_install(parsed, logs_dir):
                return (
                    manual_regression.Target("test-install", install / "chibipop.exe", True),
                    manual_regression.Result(
                        "preflight.test-install",
                        "preflight",
                        "Create disposable test install",
                        "auto",
                        "PASS",
                    ),
                )

            def fake_build_checks():
                return [
                    manual_regression.Check(
                        "x",
                        "1",
                        "Exploding check",
                        "auto-or-interactive",
                        "",
                        "",
                        auto="resources",
                    )
                ]

            def fake_run_auto(check, parsed, logs_dir, targets, points):
                raise RuntimeError("boom")

            original_parse_args = manual_regression.parse_args
            original_seed = manual_regression.seed_test_install
            original_build_checks = manual_regression.build_checks
            original_run_auto = manual_regression.run_auto
            manual_regression.parse_args = fake_parse_args
            manual_regression.seed_test_install = fake_seed_test_install
            manual_regression.build_checks = fake_build_checks
            manual_regression.run_auto = fake_run_auto
            try:
                code = manual_regression.main()
            finally:
                manual_regression.parse_args = original_parse_args
                manual_regression.seed_test_install = original_seed
                manual_regression.build_checks = original_build_checks
                manual_regression.run_auto = original_run_auto

            self.assertEqual(code, 1)
            self.assertFalse(install.exists())
            report = json.loads(args.report.read_text(encoding="utf-8"))
            statuses = {row["ident"]: row["status"] for row in report["results"]}
            self.assertEqual(statuses["internal.error"], "FAIL")
            self.assertEqual(statuses["postflight.test-install"], "PASS")

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

    def test_new_desktop_and_fresh_install_items_have_auto_handlers(self) -> None:
        checks = {check.ident: check for check in manual_regression.build_checks()}
        for ident in ["1.28.2", "1.28.3", "1.28.4"]:
            self.assertEqual(checks[ident].auto, "fresh_meikiocr_audit")
        self.assertIsNone(checks["1.28"].auto)
        self.assertIsNone(checks["1.28.7"].auto)
        for ident in ["2.11c", "2.11e"]:
            self.assertEqual(checks[ident].auto, "settings_desktop_smoke")
        self.assertIsNone(checks["2.11d"].auto)

    def test_wm_close_handler_leaves_normal_run_manual(self) -> None:
        check = manual_regression.Check(
            "2.11e",
            "2",
            "WM_CLOSE behavior",
            "auto-or-interactive",
            "",
            "",
            auto="settings_desktop_smoke",
        )
        target = manual_regression.Target("scratch", Path("scratch/chibipop.exe"), True)
        args = type("Args", (), {"repo_root": Path("."), "cargo": "cargo"})()

        def fake_seed(target_arg, parsed, logs_dir):
            return None

        class FakeProcess:
            pid = 123

            def poll(self):
                return 0

            def wait(self, timeout=None):
                return 0

            def kill(self):
                raise AssertionError("should not kill")

        class FakeDesktop:
            def wait_for_class(self, pid, class_name, timeout=10.0):
                return {"hwnd": 77, "visible": True}

            def windows_for_pid(self, pid, include_children=False):
                return []

            def post_close(self, hwnd):
                self.closed = hwnd

        original_seed = manual_regression.ensure_fixture_database
        original_launch = manual_regression.launch_logged_process
        original_desktop = manual_regression.Win32Desktop
        original_os_name = manual_regression.os.name
        manual_regression.ensure_fixture_database = fake_seed
        manual_regression.launch_logged_process = lambda cmd, cwd, logs_dir, name: (
            FakeProcess(),
            Path("log.txt"),
            type("Handle", (), {"close": lambda self: None})(),
        )
        manual_regression.Win32Desktop = FakeDesktop
        manual_regression.os.name = "nt"
        try:
            result = manual_regression.auto_settings_desktop_smoke(
                check,
                args,
                Path("."),
                [target],
            )
        finally:
            manual_regression.ensure_fixture_database = original_seed
            manual_regression.launch_logged_process = original_launch
            manual_regression.Win32Desktop = original_desktop
            manual_regression.os.name = original_os_name

        self.assertEqual(result.status, "MANUAL")
        self.assertIn("normal run route", result.detail)

    def test_settings_audit_seeds_disposable_database_first(self) -> None:
        check = manual_regression.Check(
            "1.26",
            "1",
            "Scrollable settings window",
            "auto-or-interactive",
            "",
            "",
            auto="settings_audit",
        )
        target = manual_regression.Target("scratch", Path("scratch/chibipop.exe"), True)
        args = type("Args", (), {"repo_root": Path("."), "cargo": "cargo"})()
        calls = []

        def fake_seed(target_arg, parsed, logs_dir):
            calls.append("seed")
            return manual_regression.Result(
                "preflight.fixture-db",
                "preflight",
                "Seed fixture dictionary",
                "auto",
                "PASS",
            )

        def fake_run_cmd(cmd, cwd, logs_dir, name, timeout=None):
            calls.append(name)
            data = {"controls": [{"id": 100, "rect": {"y": 10}}]}
            return 0, json.dumps(data), 0.1, Path("audit.log")

        original_seed = manual_regression.ensure_fixture_database
        original_run_cmd = manual_regression.run_cmd
        manual_regression.ensure_fixture_database = fake_seed
        manual_regression.run_cmd = fake_run_cmd
        try:
            result = manual_regression.auto_settings_audit(
                check,
                args,
                Path("."),
                [target],
            )
        finally:
            manual_regression.ensure_fixture_database = original_seed
            manual_regression.run_cmd = original_run_cmd

        self.assertEqual(result.status, "PASS")
        self.assertEqual(calls, ["seed", "tier1-1.26-settings-audit"])
        self.assertEqual(result.evidence["fixture_db"], "PASS")

    def test_source_does_not_embed_local_machine_paths(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        slash_user = "/c/" + "Users" + "/"
        win_user = "Users" + "\\" + "St" + "ella"
        nightly_name = "chibipop-" + "nightly"
        banned = [chr(67) + ":" + "\\", slash_user, win_user, nightly_name]
        self.assertFalse(any(item in source for item in banned))


if __name__ == "__main__":
    unittest.main()
