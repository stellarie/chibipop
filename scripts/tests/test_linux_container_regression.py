from __future__ import annotations

import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET

from scripts import linux_container_regression as runner


class LinuxContainerRegressionTests(unittest.TestCase):
    def make_repo(self, root: Path) -> Path:
        repo = root / "repo"
        (repo / "scripts" / "docker").mkdir(parents=True)
        (repo / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8"
        )
        (repo / "Cargo.lock").write_text("lock", encoding="utf-8")
        (repo / "scripts" / "docker" / "linux-regression.Dockerfile").write_text(
            "FROM ubuntu:24.04\n", encoding="utf-8"
        )
        return repo

    def args(self, repo: Path, artifacts: Path) -> argparse.Namespace:
        return argparse.Namespace(
            repo_root=repo.resolve(), artifacts_dir=artifacts.resolve(), loops=1,
            image="test-image", runtime="docker", timeout_seconds=10,
            platform="linux/amd64",
            cpus=2.0, memory="2g", keep_failed_container=False,
            skip_image_build=True, dry_run=False, list_schedule=False,
            artifact_source_exclude=None, source_date_epoch="1234567890",
        )

    def test_help_and_argument_validation(self) -> None:
        with self.assertRaises(SystemExit) as raised:
            runner.main(["--help"])
        self.assertEqual(raised.exception.code, 0)
        with tempfile.TemporaryDirectory() as temporary:
            repo = self.make_repo(Path(temporary))
            with self.assertRaises(SystemExit):
                runner.main(["--repo-root", str(repo), "--loops", "0"])

    def test_schedule_has_all_linux_gates(self) -> None:
        names = [step.name for step in runner.schedule("1.2.3")]
        self.assertEqual(
            [name for name in names if name.startswith("workspace-tests-")],
            ["workspace-tests-1", "workspace-tests-2", "workspace-tests-3"],
        )
        self.assertIn("ocr-quality-gate", names)
        self.assertIn("clippy-accepted", names)
        self.assertIn("clippy-suppressed", names)
        self.assertIn("release-build", names)
        for target in (
            "wayland_probe", "popup_live", "wlr_capture_live", "trigger_live",
            "portal_capture_live", "surfaces_live", "clipboard_live",
        ):
            self.assertIn(f"live-{target}", names)
        self.assertIn("degradation-no-layer-shell", names)
        self.assertIn("degradation-portal-denial", names)

    def test_live_skip_is_unavailable_and_degradation_skip_is_failure(self) -> None:
        live = runner.Step("live", ("test",), "capability")
        degradation = runner.Step("degradation", ("test",), "required-prerequisite")
        self.assertEqual(
            runner.validate_step(live, 0, "skipping: no portal")[0],
            "UNAVAILABLE",
        )
        self.assertEqual(
            runner.validate_step(degradation, 0, "degradation assertion passed")[0],
            "PASS",
        )
        self.assertEqual(
            runner.validate_step(degradation, 0, "skipping: cage missing")[0],
            "FAIL",
        )

    def test_degradation_allows_expected_unavailable_status_log(self) -> None:
        degradation = runner.Step("degradation", ("test",), "required-prerequisite")
        status, reason = runner.validate_step(
            degradation,
            0,
            "worker: unavailable - the portal capture session was refused\n"
            "test result: ok. 1 passed; 0 failed",
        )
        self.assertEqual(status, "PASS")
        self.assertIn("command completed", reason)

    def test_degradation_allows_successful_portal_denial_log(self) -> None:
        degradation = runner.Step("degradation", ("test",), "required-prerequisite")
        status, reason = runner.validate_step(
            degradation,
            0,
            "no org.freedesktop.portal.GlobalShortcuts interface exists\n"
            "degradation assertion passed\n"
            "test result: ok. 1 passed; 0 failed",
        )
        self.assertEqual(status, "PASS")
        self.assertIn("command completed", reason)

    def test_degradation_rejects_explicit_self_skip_marker(self) -> None:
        degradation = runner.Step("degradation", ("test",), "required-prerequisite")
        for output in (
            "skipping: cage missing",
            "test portal_denial ... skipped: no compositor",
            "skip: image lacks sway",
        ):
            with self.subTest(output=output):
                status, reason = runner.validate_step(degradation, 0, output)
                self.assertEqual(status, "FAIL")
                self.assertIn("self-skipped", reason)

    def test_portal_denial_keeps_required_prerequisite_gate(self) -> None:
        steps = {step.name: step for step in runner.schedule("1.2.3")}
        denial = steps["degradation-portal-denial"]
        self.assertEqual("required-prerequisite", denial.validator)
        self.assertEqual(
            ("PASS", "command completed"),
            runner.validate_step(denial, 0, "worker: unavailable - permission denied"),
        )

    def test_docker_daemon_loss_is_infrastructure_but_cargo_error_is_product(self) -> None:
        daemon = "Error response from daemon: container vanished"
        self.assertTrue(runner.docker_transport_failure(125, daemon))
        self.assertTrue(
            runner.docker_transport_failure(
                127, "OCI runtime exec failed: executable file not found in $PATH"
            )
        )
        self.assertFalse(runner.docker_transport_failure(101, "error: Rust compilation failed"))
        cargo = runner.Step("cargo", ("cargo", "test"))
        self.assertEqual(runner.validate_step(cargo, 101, "error: Rust compilation failed")[0], "FAIL")

    def test_scheduled_exec_daemon_loss_records_infrastructure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                if "product-command" in command:
                    return subprocess.CompletedProcess(
                        command, 125, "Error response from daemon: container is not running"
                    )
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ):
                result = runner.run_loop(
                    args, 1, "b" * 32,
                    [runner.Step("scheduled", ("product-command",))],
                )
        scheduled = next(step for step in result["steps"] if step["name"] == "scheduled")
        self.assertEqual(scheduled["status"], "INFRASTRUCTURE")
        self.assertIn("daemon", scheduled["reason"])

    def test_run_loop_adds_wayland_only_after_sway_starts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")
            seen: dict[str, str] = {}
            sway = next(step for step in runner.schedule("1.2.3") if step.name == "headless-sway-start")

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                rendered = " ".join(command)
                if "pre-sway-product" in command:
                    seen["pre"] = rendered
                if "set -e; mkdir -p /runtime /smoke" in rendered:
                    seen["start"] = rendered
                if "post-sway-product" in command:
                    seen["post"] = rendered
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ):
                runner.run_loop(
                    args, 1, "9" * 32,
                    [
                        runner.Step("pre", ("pre-sway-product",)),
                        sway,
                        runner.Step("post", ("post-sway-product",)),
                    ],
                )
        self.assertNotIn("WAYLAND_DISPLAY=wayland-1", seen["pre"])
        self.assertNotIn("SWAYSOCK=/runtime/sway-ipc.sock", seen["pre"])
        self.assertIn("WAYLAND_DISPLAY=wayland-1", seen["start"])
        self.assertIn("SWAYSOCK=/runtime/sway-ipc.sock", seen["start"])
        self.assertIn("WAYLAND_DISPLAY=wayland-1", seen["post"])
        self.assertIn("SWAYSOCK=/runtime/sway-ipc.sock", seen["post"])

    def test_scheduled_cargo_nonzero_remains_product_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                if "cargo-product" in command:
                    return subprocess.CompletedProcess(command, 101, "error: compilation failed")
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ):
                result = runner.run_loop(
                    args, 1, "c" * 32,
                    [runner.Step("cargo-product", ("cargo-product",))],
                )
        scheduled = next(step for step in result["steps"] if step["name"] == "cargo-product")
        self.assertEqual(scheduled["status"], "FAIL")

    def test_tool_probe_requires_each_promised_tool(self) -> None:
        probe = runner.tool_versions_command()
        self.assertIn("set -eu", probe)
        self.assertIn("for tool in rustc cargo sway cage", probe)
        self.assertIn('command -v "$tool"', probe)
        self.assertIn('"$tool" --version', probe)
        self.assertNotIn("|| true", probe)

    def test_dockerfile_does_not_force_reserved_uid(self) -> None:
        dockerfile = Path(runner.__file__).parent / "docker" / "linux-regression.Dockerfile"
        source = dockerfile.read_text(encoding="utf-8")
        self.assertIn("useradd --create-home --shell /bin/bash regression", source)
        self.assertNotIn("--uid 1000", source)
        self.assertIn("USER regression", source)

    def test_evidence_copy_failure_is_infrastructure(self) -> None:
        evidence = runner.Step("evidence", ("bash",), "evidence", required=False)
        self.assertEqual(
            runner.validate_step(evidence, 1, "cp: write error")[0],
            "INFRASTRUCTURE",
        )
        self.assertIn("set -eu", runner.evidence_stage_command())
        self.assertNotIn("|| true", runner.evidence_stage_command())

    def test_missing_optional_evidence_is_unavailable_and_cleans_up(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                if "/artifacts/compositor" in " ".join(command):
                    return subprocess.CompletedProcess(
                        command, 0, "UNAVAILABLE: no compositor evidence directory was produced"
                    )
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ) as cleanup:
                result = runner.run_loop(args, 1, "d" * 32, [])
        evidence = next(
            step for step in result["steps"] if step["name"] == "compositor-evidence-stage"
        )
        self.assertEqual(evidence["status"], "UNAVAILABLE")
        self.assertTrue(result["cleanup"]["attempted"])
        cleanup.assert_called_once()

    def test_evidence_copy_failure_preserves_token_bound_container_when_requested(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")
            args.keep_failed_container = True
            commands: list[list[str]] = []

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                commands.append(command)
                if "/artifacts/compositor" in " ".join(command):
                    return subprocess.CompletedProcess(command, 1, "cp: write error")
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup"
            ) as cleanup:
                token = "e" * 32
                result = runner.run_loop(args, 1, token, [])
        evidence = next(
            step for step in result["steps"] if step["name"] == "compositor-evidence-stage"
        )
        self.assertEqual(evidence["status"], "INFRASTRUCTURE")
        self.assertFalse(result["cleanup"]["attempted"])
        self.assertIn(token, " ".join(commands[0]))
        self.assertIn(result["container_name"], " ".join(commands[0]))
        cleanup.assert_not_called()

    def test_evidence_copy_failure_cleans_exact_container_without_keep(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                if "/artifacts/compositor" in " ".join(command):
                    return subprocess.CompletedProcess(command, 1, "cp: write error")
                return subprocess.CompletedProcess(command, 0, "ok")

            token = "f" * 32
            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ) as cleanup:
                result = runner.run_loop(args, 1, token, [])
        evidence = next(
            step for step in result["steps"] if step["name"] == "compositor-evidence-stage"
        )
        self.assertEqual(evidence["status"], "INFRASTRUCTURE")
        self.assertTrue(result["cleanup"]["attempted"])
        cleanup.assert_called_once_with("docker", result["container_name"], token)

    def test_direct_smoke_includes_real_daemon_and_bounded_shutdown(self) -> None:
        smoke = runner.direct_smoke_command()
        self.assertIn('wait_count "worker: pipeline up" 1', smoke)
        self.assertIn("trigger: frozen grab of output", smoke)
        self.assertIn("trigger: no cursor sample yet", smoke)
        self.assertIn("sleep 10; kill -KILL", smoke)
        self.assertEqual(smoke.count('stop "$'), 2)

    def test_headless_sway_start_clears_stale_runtime_sockets(self) -> None:
        start = next(step for step in runner.schedule("1.2.3") if step.name == "headless-sway-start")
        rendered = " ".join(start.command)
        self.assertIn("rm -f /runtime/wayland-1 /runtime/wayland-1.lock", rendered)
        self.assertIn("/runtime/sway-ipc.sock /runtime/sway-ipc.*.sock", rendered)
        self.assertIn("for i in $(seq 1 150)", rendered)
        self.assertIn("test -S /runtime/wayland-1 && break", rendered)
        self.assertIn("cat /smoke/sway.log; exit 1", rendered)
        self.assertNotIn("&& exit 0", rendered)

    def test_nested_artifacts_are_excluded_from_every_workspace_copy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, repo / "custom" / "nested-artifacts")
            runner.validate_args(args)
            command = runner.workspace_copy_command(args, "loop-container")
        rendered = " ".join(command)
        self.assertIn("git -c safe.directory=/source -C /source ls-files", rendered)
        self.assertIn("ls-files -z --cached -- .", rendered)
        self.assertIn("ls-files -z --others --exclude-standard -- .", rendered)
        self.assertIn("git -c safe.directory=/source checkout-index", rendered)
        self.assertIn("git -c safe.directory=/source -C /source diff --binary", rendered)
        self.assertIn("git apply --binary --whitespace=nowarn", rendered)
        self.assertIn("test -f /work/source/Cargo.toml", rendered)
        self.assertIn(":!custom/nested-artifacts", rendered)
        self.assertIn(":!work", rendered)
        self.assertIn("--user root", rendered)
        self.assertNotIn("chown", rendered)
        self.assertIn("--no-same-owner", rendered)
        self.assertIn("--no-same-permissions", rendered)
        self.assertNotIn("loop-1", rendered)
        self.assertNotIn("loop-2", rendered)

    def test_artifacts_cannot_overlap_source_or_own_repository(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            for artifacts in (repo, repo / "scripts" / "evidence", root):
                with self.subTest(artifacts=artifacts):
                    with self.assertRaises(ValueError):
                        runner.validate_args(self.args(repo, artifacts))

    def test_protected_artifact_components_are_case_insensitive(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            for component in ("SCRIPTS", "DoCs", "DATA", "TaRgEt"):
                with self.subTest(component=component), self.assertRaises(ValueError):
                    runner.validate_args(self.args(repo, repo / component / "evidence"))

    def test_repo_bind_path_rejects_commas(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "comma,parent"
            root.mkdir()
            repo = self.make_repo(root)
            with self.assertRaisesRegex(ValueError, "cannot contain a comma"):
                runner.validate_args(self.args(repo, root / "artifacts"))

    def test_source_date_epoch_is_forwarded_without_git_copy(self) -> None:
        command = runner.docker_exec_base("docker", "container", "1712345678")
        rendered = " ".join(command)
        self.assertIn("SOURCE_DATE_EPOCH=1712345678", rendered)
        self.assertIn("ICED_BACKEND=tiny-skia", rendered)
        self.assertNotIn("/.git", rendered)

    def test_pre_sway_exec_omits_wayland_client_variables(self) -> None:
        command = runner.docker_exec_base("docker", "container")
        rendered = " ".join(command)
        self.assertIn("XDG_RUNTIME_DIR=/runtime", rendered)
        self.assertIn("WLR_RENDERER=pixman", rendered)
        self.assertIn("ICED_BACKEND=tiny-skia", rendered)
        self.assertNotIn("WAYLAND_DISPLAY=wayland-1", rendered)
        self.assertNotIn("SWAYSOCK=/runtime/sway-ipc.sock", rendered)

    def test_post_sway_exec_includes_wayland_client_variables(self) -> None:
        command = runner.docker_exec_base("docker", "container", include_wayland=True)
        rendered = " ".join(command)
        self.assertIn("WAYLAND_DISPLAY=wayland-1", rendered)
        self.assertIn("SWAYSOCK=/runtime/sway-ipc.sock", rendered)

    def test_create_is_labelled_limited_and_has_no_unsafe_access(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")
            command = runner.create_command(args, "unique-container-1", "secret-token", root / "loop")
        rendered = " ".join(command)
        self.assertIn(f"{runner.TOKEN_LABEL}=secret-token", rendered)
        self.assertIn("--cpus 2.0", rendered)
        self.assertIn("--platform linux/amd64", rendered)
        self.assertIn("--memory 2g", rendered)
        self.assertIn("--cap-drop ALL", rendered)
        self.assertIn("no-new-privileges", rendered)
        self.assertIn("dst=/source,readonly", rendered)
        self.assertIn("type=volume,dst=/artifacts", rendered)
        self.assertNotIn("--privileged", rendered)
        self.assertNotIn("--network host", rendered)
        self.assertNotIn("docker.sock", rendered)
        self.assertNotIn("/home/", rendered)

    def test_unique_tokens_produce_unique_container_names(self) -> None:
        first = f"chibipop-linux-regression-{'a' * 12}-1"
        second = f"chibipop-linux-regression-{'b' * 12}-1"
        self.assertNotEqual(first, second)

    @mock.patch.object(runner.shutil, "which", return_value=None)
    def test_missing_cli_writes_json_and_junit(self, _which: mock.Mock) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            artifacts = root / "artifacts"
            result = runner.main([
                "--repo-root", str(repo), "--artifacts-dir", str(artifacts),
                "--skip-image-build",
            ])
            report = json.loads((artifacts / "linux-regression-report.json").read_text(encoding="utf-8"))
            junit = ET.parse(artifacts / "linux-regression-junit.xml")
        self.assertEqual(result, 2)
        self.assertEqual(report["schema"], runner.SCHEMA)
        self.assertEqual(report["status"], "INFRASTRUCTURE")
        self.assertEqual(report["summary"]["INFRASTRUCTURE"], 1)
        self.assertEqual(report["preflight"]["runtime_cli"], "MISSING")
        self.assertEqual(report["preflight"]["daemon"], "NOT_CHECKED")
        self.assertEqual(len(junit.getroot().findall("testcase/error")), 1)
        self.assertIn("runtime CLI not found", junit.getroot().find("testcase/error").text)

    @mock.patch.object(runner.shutil, "which", return_value="docker")
    def test_unreachable_daemon_is_distinct(self, _which: mock.Mock) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            artifacts = root / "artifacts"

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                if command[:2] == ["docker", "version"]:
                    return subprocess.CompletedProcess(command, 1, "daemon unavailable")
                return subprocess.CompletedProcess(command, 0, "")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "run_loop", return_value={"index": 1, "steps": []}
            ):
                result = runner.main([
                    "--repo-root", str(repo), "--artifacts-dir", str(artifacts),
                    "--skip-image-build",
                ])
            report = json.loads((artifacts / "linux-regression-report.json").read_text(encoding="utf-8"))
            junit = ET.parse(artifacts / "linux-regression-junit.xml")
        self.assertEqual(result, 2)
        self.assertEqual(report["status"], "INFRASTRUCTURE")
        self.assertEqual(report["summary"]["INFRASTRUCTURE"], 1)
        self.assertEqual(report["preflight"]["runtime_cli"], "FOUND")
        self.assertEqual(report["preflight"]["daemon"], "UNREACHABLE")
        self.assertEqual(len(junit.getroot().findall("testcase/error")), 1)
        self.assertEqual(junit.getroot().find("testcase/error").text, "daemon unavailable")

    @mock.patch.object(runner.shutil, "which", return_value="podman")
    def test_podman_daemon_probe_uses_runtime_neutral_version(
        self, _which: mock.Mock
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            artifacts = root / "artifacts"
            commands: list[list[str]] = []

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                commands.append(command)
                if command[:2] == ["podman", "version"]:
                    return subprocess.CompletedProcess(command, 0, '"5.8.3"')
                if command[:2] == ["podman", "image"]:
                    return subprocess.CompletedProcess(command, 0, "sha256:image")
                return subprocess.CompletedProcess(command, 0, "ok")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "run_loop", return_value={"index": 1, "steps": []}
            ):
                result = runner.main([
                    "--repo-root", str(repo), "--artifacts-dir", str(artifacts),
                    "--runtime", "podman", "--skip-image-build",
                ])
            report = json.loads((artifacts / "linux-regression-report.json").read_text(encoding="utf-8"))
        self.assertEqual(result, 0)
        runtime_commands = [command for command in commands if command and command[0] == "podman"]
        self.assertEqual(runtime_commands[0][:4], ["podman", "version", "--format", "{{json .Server.Version}}"])
        self.assertNotIn(["podman", "info"], [command[:2] for command in runtime_commands])
        self.assertEqual(report["preflight"]["daemon"], "REACHABLE")
        self.assertEqual(report["preflight"]["server_version"], '"5.8.3"')

    def test_daemon_probe_falls_back_only_for_template_errors(self) -> None:
        calls: list[list[str]] = []

        def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
            calls.append(command)
            if command[:2] == ["podman", "version"]:
                return subprocess.CompletedProcess(command, 1, "template: version: executing: can't evaluate field Server")
            return subprocess.CompletedProcess(command, 0, '"5.8.3"')

        with mock.patch.object(runner, "run_process", side_effect=fake):
            result = runner.runtime_daemon_probe("podman")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, '"5.8.3"')
        self.assertEqual([command[:2] for command in calls], [["podman", "version"], ["podman", "info"]])

    @mock.patch.object(runner.shutil, "which", return_value="docker")
    def test_image_build_failure_and_timeout_count_one_preflight_error(
        self, _which: mock.Mock
    ) -> None:
        for mode in ("failure", "timeout"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                repo = self.make_repo(root)
                artifacts = root / "artifacts"

                def fake(
                    command: list[str], timeout: int | None = None
                ) -> subprocess.CompletedProcess[str]:
                    if command[:2] == ["docker", "version"]:
                        return subprocess.CompletedProcess(command, 0, '"27.0"')
                    if command[:2] == ["docker", "build"]:
                        if mode == "timeout":
                            raise subprocess.TimeoutExpired(command, timeout or 1, output="partial build")
                        return subprocess.CompletedProcess(command, 1, "build failed")
                    return subprocess.CompletedProcess(command, 0, "metadata")

                with mock.patch.object(runner, "run_process", side_effect=fake):
                    result = runner.main([
                        "--repo-root", str(repo), "--artifacts-dir", str(artifacts),
                    ])
                report = json.loads(
                    (artifacts / "linux-regression-report.json").read_text(encoding="utf-8")
                )
                junit = ET.parse(artifacts / "linux-regression-junit.xml")
                self.assertEqual(result, 1)
                self.assertEqual(report["status"], "INFRASTRUCTURE")
                self.assertEqual(report["summary"]["INFRASTRUCTURE"], 1)
                self.assertEqual(len(junit.getroot().findall("testcase/error")), 1)
                expected = "timed out" if mode == "timeout" else "exit code 1"
                self.assertIn(expected, junit.getroot().find("testcase/error").text)

    def test_cleanup_requires_matching_token(self) -> None:
        mismatch = subprocess.CompletedProcess([], 0, "other-token\n")
        with mock.patch.object(runner, "run_process", return_value=mismatch) as called:
            ok, detail = runner.exact_cleanup("docker", "container", "expected-token")
        self.assertFalse(ok)
        self.assertIn("mismatch", detail)
        self.assertEqual(called.call_count, 1)

    def test_cleanup_force_removes_only_after_matching_token(self) -> None:
        responses = [
            subprocess.CompletedProcess([], 0, "expected-token\n"),
            subprocess.CompletedProcess([], 0, "container\n"),
        ]
        with mock.patch.object(runner, "run_process", side_effect=responses) as called:
            ok, _detail = runner.exact_cleanup("docker", "container", "expected-token")
        self.assertTrue(ok)
        self.assertEqual(
            called.call_args_list[1].args[0],
            ["docker", "rm", "--force", "--volumes", "container"],
        )

    def test_timeout_is_a_failure_and_cleanup_still_runs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            args = self.args(repo, root / "artifacts")
            calls = 0

            def fake(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
                nonlocal calls
                calls += 1
                if calls <= 4:
                    outputs = ["id", "started", "copied", "versions"]
                    return subprocess.CompletedProcess(command, 0, outputs[calls - 1])
                raise subprocess.TimeoutExpired(command, timeout or 1, output="partial")

            with mock.patch.object(runner, "run_process", side_effect=fake), mock.patch.object(
                runner, "exact_cleanup", return_value=(True, "removed")
            ) as cleanup:
                result = runner.run_loop(args, 1, "a" * 32, [runner.Step("slow", ("slow",))])
        slow = next(step for step in result["steps"] if step["name"] == "slow")
        self.assertEqual(slow["status"], "INFRASTRUCTURE")
        self.assertIn("timed out", slow["reason"])
        self.assertEqual(slow["output_tail"], "partial")
        self.assertTrue(slow["command"])
        self.assertGreaterEqual(slow["duration_seconds"], 0)
        cleanup.assert_called_once()

    def test_each_lifecycle_timeout_keeps_partial_output_and_log(self) -> None:
        names = (
            "container-create", "container-start", "workspace-copy",
            "tool-versions", "container-artifact-export",
        )
        with tempfile.TemporaryDirectory() as temporary:
            loop_dir = Path(temporary)
            for name in names:
                with self.subTest(name=name), mock.patch.object(
                    runner,
                    "run_process",
                    side_effect=subprocess.TimeoutExpired(
                        ["docker", name], 3, output=f"partial-{name}"
                    ),
                ):
                    code, output, record = runner.run_lifecycle(
                        name, ["docker", name], 3, loop_dir
                    )
                self.assertEqual(code, -1)
                self.assertEqual(output, f"partial-{name}")
                self.assertEqual(record["status"], "INFRASTRUCTURE")
                self.assertEqual(record["command"], ["docker", name])
                self.assertIn(f"partial-{name}", record["output_tail"])
                self.assertTrue((loop_dir / f"lifecycle-{name}.log").is_file())

    def test_failed_command_survives_in_json_and_junit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifacts = Path(temporary)
            args = argparse.Namespace(
                loops=1, image="image", runtime="docker", timeout_seconds=10,
                platform="linux/amd64",
                cpus=1.0, memory="1g", keep_failed_container=False,
                skip_image_build=True, repo_root=artifacts,
            )
            report = runner.empty_report(args)
            report["loops"] = [{
                "index": 1,
                "steps": [{
                    "name": "broken", "status": "FAIL", "reason": "exit code 9",
                    "duration_seconds": 0.1, "output_tail": "failure evidence",
                }],
            }]
            runner.update_summary(report)
            runner.write_outputs(report, artifacts)
            saved = json.loads((artifacts / "linux-regression-report.json").read_text(encoding="utf-8"))
            junit = ET.parse(artifacts / "linux-regression-junit.xml")
        self.assertEqual(saved["summary"]["FAIL"], 1)
        self.assertEqual(junit.getroot().find("testcase/failure").text, "failure evidence")

    def test_infrastructure_failure_is_a_junit_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            artifacts = Path(temporary)
            args = argparse.Namespace(
                loops=1, image="image", runtime="docker", timeout_seconds=10,
                platform="linux/amd64", cpus=1.0, memory="1g",
                keep_failed_container=False, skip_image_build=True,
                repo_root=artifacts,
            )
            report = runner.empty_report(args)
            report["loops"] = [{
                "index": 1,
                "steps": [{
                    "name": "docker-timeout", "status": "INFRASTRUCTURE",
                    "reason": "timed out", "duration_seconds": 1.0,
                    "output_tail": "partial daemon output",
                }],
            }]
            runner.update_summary(report)
            runner.write_outputs(report, artifacts)
            junit = ET.parse(artifacts / "linux-regression-junit.xml")
        self.assertEqual(report["status"], "INFRASTRUCTURE")
        self.assertEqual(report["summary"]["INFRASTRUCTURE"], 1)
        self.assertEqual(junit.getroot().find("testcase/error").text, "partial daemon output")
        self.assertEqual(len(junit.getroot().findall("testcase/error")), 1)

    def test_dry_run_does_not_contact_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            output = io.StringIO()
            with contextlib.redirect_stdout(output), mock.patch.object(
                runner, "run_process"
            ) as process:
                result = runner.main([
                    "--repo-root", str(repo), "--artifacts-dir", str(root / "artifacts"),
                    "--dry-run",
                ])
        self.assertEqual(result, 0)
        self.assertIn("not a VM", output.getvalue())
        self.assertIn("live-wayland_probe", output.getvalue())
        process.assert_not_called()

    def test_report_schema(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = self.make_repo(root)
            report = runner.empty_report(self.args(repo, root / "artifacts"))
        self.assertEqual(report["schema"], "chibipop-linux-regression-report/v1")
        self.assertEqual(
            set(report["summary"]),
            {"PASS", "FAIL", "SKIP", "UNAVAILABLE", "INFRASTRUCTURE"},
        )

    def test_source_has_no_shell_true_or_machine_specific_paths(self) -> None:
        source = Path(runner.__file__).read_text(encoding="utf-8")
        dockerfile = Path(runner.__file__).parent / "docker" / "linux-regression.Dockerfile"
        combined = source + dockerfile.read_text(encoding="utf-8")
        self.assertNotIn("shell=True", combined)
        self.assertNotRegex(combined, r"[A-Za-z]:\\\\Users\\\\")
        self.assertNotIn("/var/run/docker.sock", combined)


if __name__ == "__main__":
    unittest.main()
