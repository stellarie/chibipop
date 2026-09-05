#!/usr/bin/env python3
"""Run Chibipop's Linux regression schedule in isolated Docker containers.

Docker supplies a Linux container, not a virtual machine. Each loop receives
fresh writable volumes while the host checkout is mounted read-only.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import time
import uuid
import xml.etree.ElementTree as ET


SCHEMA = "chibipop-linux-regression-report/v1"
TOKEN_LABEL = "io.chibipop.regression.token"
LOOP_LABEL = "io.chibipop.regression.loop"
UNAVAILABLE_RE = re.compile(
    r"\b(skip(?:ped|ping)?|unavailable|not available|cannot run|no portal)\b",
    re.IGNORECASE,
)
PREREQUISITE_SKIP_RE = re.compile(
    r"(?im)^\s*(?:test\s+\S+\s+\.\.\.\s+)?(?:skip|skipped|skipping):\s+\S",
    re.IGNORECASE,
)
DOCKER_TRANSPORT_RE = re.compile(
    r"(?:error response from daemon|cannot connect to (?:the )?docker daemon|"
    r"context deadline exceeded|error during connect|no such container|"
    r"container .* is not running|docker daemon is not running|"
    r"oci runtime exec failed|failed to create task for container|"
    r"executable file not found in \$path)",
    re.IGNORECASE,
)
RUNTIME_PROBE_TEMPLATE_RE = re.compile(
    r"(?:can't evaluate field|map has no entry for key|template:.*executing)",
    re.IGNORECASE,
)


@dataclasses.dataclass(frozen=True)
class Step:
    name: str
    command: tuple[str, ...]
    validator: str = "exit"
    required: bool = True


def direct_smoke_command() -> str:
    return r'''set -euo pipefail
BIN=/cargo-target/debug/chibipop
export XDG_CONFIG_HOME=/smoke/config XDG_DATA_HOME=/smoke/data
export XDG_STATE_HOME=/smoke/state XDG_CACHE_HOME=/smoke/cache
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
log=/smoke/state/chibipop/chibipop.log
fail() { echo "$*"; cat "$log" 2>/dev/null || true; exit 1; }
count() { c=$(grep -cF "$1" "$log" 2>/dev/null || true); echo "${c:-0}"; }
wait_count() {
  for i in $(seq 1 600); do
    test "$(count "$1")" -ge "$2" && return 0
    sleep .1
  done
  fail "waited for occurrence $2 of $1"
}
grab() {
  mkdir -p "/smoke/$1"
  "$BIN" capture-dump --region "$2" --out "/smoke/$1"
}
stop() {
  target=$1
  kill -TERM "$target"
  (sleep 10; kill -KILL "$target" 2>/dev/null || true) &
  watchdog=$!
  set +e
  wait "$target"
  status=$?
  set -e
  kill "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  case "$status" in 0|143) ;; *) fail "daemon exit status $status" ;; esac
}
CHIBIPOP_POPUP_DEMO=1 CHIBIPOP_POPUP_DEMO_ANCHOR=400,300,140,40 "$BIN" run &
demo=$!
trap 'kill "$demo" "${real:-}" 2>/dev/null || true' EXIT
wait_count "layer surface(s) mapped hidden" 1
grep -F "popup: layer surface 0 on " "$log" >/dev/null || fail "no layer surface"
grep -F "keyboard none" "$log" >/dev/null || fail "popup took keyboard focus"
grep -F "capture: wlr-screencopy" "$log" >/dev/null || fail "wrong capture rung"
"$BIN" ctl trigger-down
wait_count "popup: shown on surface" 1
shown=$(grep -F "popup: shown on surface" "$log" | tail -1)
grep -F "font: painting with" "$log" >/dev/null || fail "font was not recorded"
geom=$(printf '%s\n' "$shown" | grep -oE '[0-9-]+,[0-9-]+ [0-9]+x[0-9]+' | head -1 | tr ' x' ',,')
test -n "$geom" || fail "could not parse popup geometry"
"$BIN" ctl trigger-up
wait_count "popup: hidden in" 1
grab hidden-before "$geom"
"$BIN" ctl trigger-down
wait_count "popup: shown on surface" 2
grab shown "$geom"
"$BIN" ctl trigger-up
wait_count "popup: hidden in" 2
grab hidden-after "$geom"
test ! -e /smoke/shown/chibipop-capture-0.png && fail "shown PNG missing"
cmp -s /smoke/hidden-before/chibipop-capture-0.png /smoke/shown/chibipop-capture-0.png \
  && fail "shown and hidden pixels were identical"
cmp /smoke/hidden-before/chibipop-capture-0.png /smoke/hidden-after/chibipop-capture-0.png \
  || fail "hide did not restore the pixels"
stop "$demo"
rm -f "$log"
"$BIN" run &
real=$!
wait_count "worker: pipeline up" 1
"$BIN" ctl trigger-down
for i in $(seq 1 300); do
  test "$(count 'trigger: frozen grab of output')" -ge 1 && break
  test "$(count 'trigger: no cursor sample yet')" -ge 1 && break
  sleep .1
done
if test "$(count 'trigger: frozen grab of output')" -ge 1; then
  grep -F "lookup failed" "$log" >/dev/null && fail "lookup behind frozen grab failed"
  grep -E "lookup|no dictionary at" "$log" >/dev/null \
    || fail "frozen grab had no lookup or no-dictionary outcome"
elif test "$(count 'trigger: no cursor sample yet')" -ge 1; then
  echo "UNAVAILABLE: trigger lookup has no cursor rung"
else
  fail "trigger produced neither a frozen grab nor a no-cursor diagnostic"
fi
"$BIN" ctl trigger-up
kill -0 "$real" || fail "non-demo daemon died during trigger round-trip"
stop "$real"
trap - EXIT
echo "direct popup, screencopy, control, and real trigger smoke passed"
'''


def tool_versions_command() -> str:
    return r'''set -eu
. /etc/os-release
printf 'os=%s\n' "$PRETTY_NAME"
for tool in rustc cargo sway cage; do
  command -v "$tool" >/dev/null
  if test "$tool" = cage; then
    "$tool" --version 2>/dev/null || "$tool" -v
  else
    "$tool" --version
  fi
done
'''


def evidence_stage_command() -> str:
    return r'''set -eu
mkdir -p /artifacts/compositor
if test -d /smoke; then
  cp -a /smoke/. /artifacts/compositor/
else
  echo "UNAVAILABLE: no compositor evidence directory was produced"
fi
'''


def schedule(version: str = "0.0.0") -> list[Step]:
    common_tests = (
        "cargo", "test", "--workspace", "--exclude", "chibipop-windows",
    )
    live = [
        "wayland_probe",
        "popup_live",
        "wlr_capture_live",
        "trigger_live",
        "portal_capture_live",
        "surfaces_live",
        "clipboard_live",
    ]
    steps = [
        Step(f"workspace-tests-{index}", common_tests, "test-floor")
        for index in range(1, 4)
    ]
    steps.extend(
        [
            Step(
                "ocr-quality-gate",
                ("cargo", "test", "-p", "chibipop-linux", "--test", "ocr_gate", "--", "--nocapture"),
            ),
            Step(
                "clippy-accepted",
                ("cargo", "clippy", "--workspace", "--color", "never", "--all-targets", "--all-features"),
                "clippy-one",
            ),
            Step(
                "clippy-suppressed",
                (
                    "cargo", "clippy", "--workspace", "--color", "never", "--all-targets",
                    "--all-features", "--", "-D", "warnings", "-A", "clippy::while_let_loop",
                    "-A", "clippy::doc_lazy_continuation", "-A", "clippy::useless_conversion",
                    "-A", "clippy::too_many_arguments", "-A", "clippy::needless_lifetimes",
                    "-A", "clippy::type_complexity",
                ),
                "clippy-zero",
            ),
            Step(
                "release-build",
                ("cargo", "build", "--release", "--workspace", "--exclude", "chibipop-windows"),
            ),
            Step(
                "tarball-layout-test",
                ("cargo", "test", "-p", "chibipop-linux", "--test", "tarball_layout", "--", "--nocapture"),
            ),
            Step(
                "package-linux",
                (
                    "bash", "-lc",
                    f"OUT=/artifacts/package bash scripts/package-linux.sh v{version} /cargo-target/release/chibipop",
                ),
            ),
            Step(
                "debug-live-build",
                ("cargo", "build", "-p", "chibipop-linux", "--bin", "chibipop"),
            ),
            Step(
                "headless-sway-start",
                (
                    "bash", "-lc",
                    "set -e; mkdir -p /runtime /smoke; chmod 700 /runtime; "
                    "rm -f /runtime/wayland-1 /runtime/wayland-1.lock "
                    "/runtime/sway-ipc.sock /runtime/sway-ipc.*.sock; "
                    "printf '%s\\n' 'output HEADLESS-1 resolution 1920x1080 position 0 0' "
                    "'xwayland disable' > /smoke/sway.conf; "
                    "nohup sway -c /smoke/sway.conf >/smoke/sway.log 2>&1 & "
                    "for i in $(seq 1 150); do test -S /runtime/wayland-1 && break; sleep .2; done; "
                    "test -S /runtime/wayland-1 || { cat /smoke/sway.log; exit 1; }",
                ),
            ),
            Step(
                "headless-sway-capability-probe",
                (
                    "bash", "-lc",
                    "/cargo-target/debug/chibipop probe | tee /smoke/probe.txt && "
                    "for g in zwlr_layer_shell_v1 wp_fractional_scale_manager_v1 wp_viewporter "
                    "zwlr_screencopy_manager_v1; do grep -q \"$g v\" /smoke/probe.txt || exit 1; done",
                ),
            ),
        ]
    )
    steps.extend(
        Step(
            f"live-{test}",
            ("cargo", "test", "-p", "chibipop-linux", "--test", test, "--", "--nocapture", "--test-threads=1"),
            "capability",
        )
        for test in live
    )
    steps.extend(
        [
            Step(
                "degradation-no-layer-shell",
                ("cargo", "test", "-p", "chibipop-linux", "--test", "no_layer_shell", "--", "--nocapture", "--test-threads=1"),
                "required-prerequisite",
            ),
            Step(
                "degradation-portal-denial",
                ("cargo", "test", "-p", "chibipop-linux", "--test", "portal_denial", "--", "--nocapture", "--test-threads=1"),
                "required-prerequisite",
            ),
            Step(
                "direct-control-popup-screencopy-smoke",
                ("bash", "-lc", direct_smoke_command()),
                "capability",
            ),
            Step(
                "collect-compositor-evidence",
                ("bash", "-lc", evidence_stage_command()),
                "evidence",
                required=False,
            ),
        ]
    )
    return steps


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def run_process(command: list[str], timeout: int | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        check=False,
    )


def timeout_output(error: subprocess.TimeoutExpired) -> str:
    raw = error.stdout or error.output or ""
    return raw.decode(errors="replace") if isinstance(raw, bytes) else str(raw)


def run_lifecycle(
    name: str,
    command: list[str],
    timeout: int,
    loop_dir: Path,
) -> tuple[int, str, dict[str, object]]:
    started = time.monotonic()
    try:
        completed = run_process(command, timeout)
        code = completed.returncode
        output = completed.stdout
        reason = "operation completed" if code == 0 else f"exit code {code}"
    except subprocess.TimeoutExpired as error:
        code = -1
        output = timeout_output(error)
        reason = f"timed out after {timeout} seconds"
    except OSError as error:
        code = -1
        output = str(error)
        reason = f"could not start: {error}"
    duration = time.monotonic() - started
    log_name = f"lifecycle-{name}.log"
    (loop_dir / log_name).write_text(output, encoding="utf-8", errors="replace")
    record: dict[str, object] = {
        "name": name,
        "status": "PASS" if code == 0 else "INFRASTRUCTURE",
        "reason": reason,
        "command": command,
        "returncode": code,
        "duration_seconds": duration,
        "log": str(loop_dir.name + "/" + log_name),
        "output_tail": output[-4000:],
        "kind": "lifecycle",
    }
    return code, output, record


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Run Linux regressions in disposable Docker containers (not VMs).",
    )
    result.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    result.add_argument("--artifacts-dir", type=Path, default=Path("linux-regression-artifacts"))
    result.add_argument("--loops", type=int, default=1)
    result.add_argument("--image", default="chibipop-linux-regression:ubuntu-24.04")
    result.add_argument("--runtime", default="docker")
    result.add_argument("--platform", default="linux/amd64")
    result.add_argument("--timeout-seconds", type=int, default=1800)
    result.add_argument("--cpus", type=float, default=4.0)
    result.add_argument("--memory", default="8g")
    result.add_argument("--keep-failed-container", action="store_true")
    result.add_argument("--skip-image-build", action="store_true")
    result.add_argument("--dry-run", action="store_true")
    result.add_argument("--list", action="store_true", dest="list_schedule")
    return result


def validate_args(args: argparse.Namespace) -> None:
    if args.loops < 1:
        raise ValueError("--loops must be positive")
    if args.timeout_seconds < 1:
        raise ValueError("--timeout-seconds must be positive")
    if args.cpus <= 0:
        raise ValueError("--cpus must be positive")
    if not re.fullmatch(r"[1-9][0-9]*(?:[kKmMgG])?", args.memory):
        raise ValueError("--memory must look like 512m or 8g")
    args.repo_root = args.repo_root.resolve()
    if not (args.repo_root / "Cargo.toml").is_file():
        raise ValueError("--repo-root must contain Cargo.toml")
    if "," in str(args.repo_root):
        raise ValueError("--repo-root cannot contain a comma because Docker --mount uses CSV syntax")
    if not args.artifacts_dir.is_absolute():
        args.artifacts_dir = (args.repo_root / args.artifacts_dir).resolve()
    else:
        args.artifacts_dir = args.artifacts_dir.resolve()
    try:
        repo_from_artifacts = args.repo_root.relative_to(args.artifacts_dir)
    except ValueError:
        repo_from_artifacts = None
    if repo_from_artifacts is not None:
        raise ValueError("--artifacts-dir cannot be the repository root or its ancestor")
    try:
        relative_artifacts = args.artifacts_dir.relative_to(args.repo_root)
    except ValueError:
        relative_artifacts = None
    if relative_artifacts is not None:
        protected = {
            ".github", "cargo.toml", "cargo.lock", "crates", "data", "docs",
            "extras", "packaging", "scripts", "src", "target",
        }
        if relative_artifacts.parts and relative_artifacts.parts[0].casefold() in protected:
            raise ValueError("--artifacts-dir overlaps repository source inputs")
        args.artifact_source_exclude = "./" + relative_artifacts.as_posix()
    else:
        args.artifact_source_exclude = None
    args.source_date_epoch = "0"


def workspace_version(repo: Path) -> str:
    text = (repo / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r"(?ms)^\[workspace\.package\].*?^version\s*=\s*\"([^\"]+)\"", text)
    return match.group(1) if match else "0.0.0"


def cargo_lock_hash(repo: Path) -> str | None:
    path = repo / "Cargo.lock"
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def host_metadata(repo: Path) -> dict[str, object]:
    metadata: dict[str, object] = {"cargo_lock_sha256": cargo_lock_hash(repo)}
    for key, command in (
        ("git_revision", ["git", "-C", str(repo), "rev-parse", "HEAD"]),
        ("git_status", ["git", "-C", str(repo), "status", "--porcelain"]),
        ("source_date_epoch", ["git", "-C", str(repo), "log", "-1", "--format=%ct"]),
    ):
        try:
            completed = run_process(command, 30)
            metadata[key] = completed.stdout.strip() if completed.returncode == 0 else None
        except (OSError, subprocess.TimeoutExpired):
            metadata[key] = None
    metadata["git_dirty"] = bool(metadata.get("git_status"))
    return metadata


def empty_report(args: argparse.Namespace) -> dict[str, object]:
    return {
        "schema": SCHEMA,
        "started_at": utc_now(),
        "finished_at": None,
        "status": "RUNNING",
        "preflight": {},
        "configuration": {
            "loops": args.loops,
            "image": args.image,
            "runtime": args.runtime,
            "platform": args.platform,
            "timeout_seconds": args.timeout_seconds,
            "cpus": args.cpus,
            "memory": args.memory,
            "keep_failed_container": args.keep_failed_container,
            "skip_image_build": args.skip_image_build,
        },
        "metadata": host_metadata(args.repo_root),
        "loops": [],
        "summary": {"PASS": 0, "FAIL": 0, "SKIP": 0, "UNAVAILABLE": 0, "INFRASTRUCTURE": 0},
    }


def write_outputs(report: dict[str, object], artifacts: Path) -> None:
    artifacts.mkdir(parents=True, exist_ok=True)
    (artifacts / "linux-regression-report.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8",
    )
    suite = ET.Element("testsuite", name="chibipop-linux-container-regression")
    cases = []
    for loop in report.get("loops", []):
        for step in loop.get("steps", []):
            case = ET.SubElement(
                suite,
                "testcase",
                classname=f"linux-container.loop-{loop['index']}",
                name=step["name"],
                time=f"{step.get('duration_seconds', 0):.3f}",
            )
            if step["status"] == "FAIL":
                failure = ET.SubElement(case, "failure", message=step.get("reason", "command failed"))
                failure.text = step.get("output_tail", "")
            elif step["status"] == "INFRASTRUCTURE":
                error = ET.SubElement(case, "error", message=step.get("reason", "infrastructure failure"))
                error.text = step.get("output_tail", "")
            elif step["status"] in {"SKIP", "UNAVAILABLE"}:
                ET.SubElement(case, "skipped", message=step.get("reason", step["status"]))
            cases.append(case)
    preflight = report.get("preflight", {})
    if report.get("status") in {"FAIL", "INFRASTRUCTURE"} and not cases:
        case = ET.SubElement(suite, "testcase", classname="linux-container", name="preflight")
        detail = str(preflight.get("error", "preflight failed"))
        error = ET.SubElement(case, "error", message=detail)
        error.text = detail
        cases.append(case)
    suite.set("tests", str(len(cases)))
    suite.set("failures", str(sum(1 for c in cases if c.find("failure") is not None)))
    suite.set("errors", str(sum(1 for c in cases if c.find("error") is not None)))
    suite.set("skipped", str(sum(1 for c in cases if c.find("skipped") is not None)))
    ET.ElementTree(suite).write(artifacts / "linux-regression-junit.xml", encoding="utf-8", xml_declaration=True)


def mark_preflight_infrastructure(report: dict[str, object], error: str | None = None) -> None:
    report["status"] = "INFRASTRUCTURE"
    report["summary"]["INFRASTRUCTURE"] = 1
    if error is not None:
        report["preflight"]["error"] = error


def docker_exec_base(
    runtime: str,
    name: str,
    source_date_epoch: str = "0",
    user: str | None = None,
    include_wayland: bool = False,
) -> list[str]:
    env = {
        "CARGO_HOME": "/cargo-home",
        "CARGO_TARGET_DIR": "/cargo-target",
        "XDG_RUNTIME_DIR": "/runtime",
        "WLR_BACKENDS": "headless",
        "WLR_LIBINPUT_NO_DEVICES": "1",
        "WLR_RENDERER": "pixman",
        "ICED_BACKEND": "tiny-skia",
        "SOURCE_DATE_EPOCH": source_date_epoch,
    }
    if include_wayland:
        env["WAYLAND_DISPLAY"] = "wayland-1"
        env["SWAYSOCK"] = "/runtime/sway-ipc.sock"
    command = [runtime, "exec"]
    if user is not None:
        command.extend(["--user", user])
    command.extend(["--workdir", "/work/source"])
    for key, value in env.items():
        command.extend(["--env", f"{key}={value}"])
    command.append(name)
    return command


def validate_step(step: Step, code: int, output: str) -> tuple[str, str]:
    if code != 0:
        if step.validator == "evidence":
            return "INFRASTRUCTURE", f"evidence staging exited {code}"
        return ("FAIL" if step.required else "UNAVAILABLE", f"exit code {code}")
    if step.validator == "test-floor":
        passed = sum(int(value) for value in re.findall(r"^test result: ok\. ([0-9]+) passed", output, re.MULTILINE))
        return ("PASS", f"{passed} passing tests") if passed >= 600 else ("FAIL", f"expected at least 600 passing tests, got {passed}")
    warnings = [
        line for line in output.splitlines()
        if line.startswith("warning") and not re.match(r"warning: .*generated [0-9]+ warning", line)
    ]
    if step.validator == "clippy-one":
        return ("PASS", "exactly one accepted finding") if len(warnings) == 1 else ("FAIL", f"expected 1 accepted finding, got {len(warnings)}")
    if step.validator == "clippy-zero":
        diagnostics = [line for line in output.splitlines() if re.match(r"^(error|warning)", line)]
        return ("PASS", "zero remaining findings") if not diagnostics else ("FAIL", f"expected 0 findings, got {len(diagnostics)}")
    if step.validator == "required-prerequisite" and PREREQUISITE_SKIP_RE.search(output):
        return "FAIL", "required image prerequisite self-skipped"
    if step.validator == "evidence" and UNAVAILABLE_RE.search(output):
        return "UNAVAILABLE", "optional evidence was not produced"
    if step.validator == "capability" and UNAVAILABLE_RE.search(output):
        return "UNAVAILABLE", "test reported an unavailable capability"
    return "PASS", "command completed"


def docker_transport_failure(code: int, output: str) -> bool:
    return code != 0 and bool(DOCKER_TRANSPORT_RE.search(output))


def runtime_daemon_probe(runtime: str) -> subprocess.CompletedProcess[str]:
    probes = (
        [runtime, "version", "--format", "{{json .Server.Version}}"],
        [runtime, "info", "--format", "{{json .ServerVersion}}"],
    )
    template_errors: list[str] = []
    for command in probes:
        try:
            result = run_process(command, 30)
        except (OSError, subprocess.TimeoutExpired) as error:
            return subprocess.CompletedProcess(command, 1, str(error))
        if result.returncode == 0:
            return result
        output = result.stdout.strip()
        if not RUNTIME_PROBE_TEMPLATE_RE.search(output):
            return result
        template_errors.append(output)
    return subprocess.CompletedProcess(probes[-1], 1, "\n".join(template_errors))


def exact_cleanup(runtime: str, name: str, token: str) -> tuple[bool, str]:
    try:
        inspected = run_process(
            [runtime, "inspect", "--format", f'{{{{ index .Config.Labels "{TOKEN_LABEL}" }}}}', name],
            30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, f"cleanup inspection failed: {error}"
    if inspected.returncode != 0:
        return True, "container already absent"
    if inspected.stdout.strip() != token:
        return False, "cleanup refused: token label mismatch"
    try:
        removed = run_process([runtime, "rm", "--force", "--volumes", name], 60)
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, f"cleanup failed: {error}"
    return removed.returncode == 0, removed.stdout.strip()


def create_command(args: argparse.Namespace, name: str, token: str, loop_dir: Path) -> list[str]:
    mounts = [
        f"type=bind,src={args.repo_root},dst=/source,readonly",
        "type=volume,dst=/work",
        "type=volume,dst=/cargo-home",
        "type=volume,dst=/cargo-target",
        "type=volume,dst=/artifacts",
    ]
    command = [
        args.runtime, "create", "--name", name,
        "--platform", args.platform,
        "--label", f"{TOKEN_LABEL}={token}",
        "--label", f"{LOOP_LABEL}={name.rsplit('-', 1)[-1]}",
        "--cpus", str(args.cpus), "--memory", args.memory,
        "--pids-limit", "1024", "--security-opt", "no-new-privileges",
        "--cap-drop", "ALL",
    ]
    for mount in mounts:
        command.extend(["--mount", mount])
    command.append(args.image)
    return command


def workspace_copy_command(args: argparse.Namespace, name: str) -> list[str]:
    transient_dirs = [".claude", "target", "regression-artifacts", "linux-regression-artifacts", ".scratch", "work"]
    if args.artifact_source_exclude:
        transient_dirs.append(args.artifact_source_exclude.removeprefix("./"))
    pathspec_excludes = " ".join(shlex.quote(f":!{value}") for value in transient_dirs)
    return docker_exec_base(args.runtime, name, args.source_date_epoch, user="root") + [
        "bash", "-lc",
        "set -euo pipefail; "
        "untracked=/tmp/chibipop-untracked.$$; "
        "trap 'rm -f /tmp/chibipop-untracked.$$' EXIT; "
        "mkdir -p /work/source && "
        f"git -c safe.directory=/source -C /source ls-files -z --cached -- . {pathspec_excludes} "
        "| (cd /source && git -c safe.directory=/source checkout-index --force --stdin -z --prefix=/work/source/) && "
        f"git -c safe.directory=/source -C /source diff --binary -- . {pathspec_excludes} "
        "| (cd /work/source && git apply --binary --whitespace=nowarn) && "
        f"git -c safe.directory=/source -C /source ls-files -z --others --exclude-standard -- . {pathspec_excludes} "
        "> \"$untracked\"; "
        "if [ -s \"$untracked\" ]; then "
        "tar -C /source --null -T \"$untracked\" -cf - "
        "| tar --no-same-owner --no-same-permissions -C /work/source -xf -; "
        "fi; "
        "test -f /work/source/Cargo.toml",
    ]


def run_loop(args: argparse.Namespace, index: int, token: str, steps: list[Step]) -> dict[str, object]:
    name = f"chibipop-linux-regression-{token[:12]}-{index}"
    loop_dir = args.artifacts_dir / f"loop-{index}"
    loop_dir.mkdir(parents=True, exist_ok=True)
    result: dict[str, object] = {
        "index": index, "container_name": name, "token": token,
        "started_at": utc_now(), "finished_at": None, "steps": [], "cleanup": {},
    }
    failed = False
    created = False
    try:
        create_code, _create_output, create_record = run_lifecycle(
            "container-create", create_command(args, name, token, loop_dir), 120, loop_dir
        )
        result["steps"].append(create_record)
        if create_code != 0:
            result["steps"].extend(
                {"name": step.name, "status": "SKIP", "reason": "container creation failed", "duration_seconds": 0.0}
                for step in steps
            )
            return result
        created = True
        start_code, _start_output, start_record = run_lifecycle(
            "container-start", [args.runtime, "start", name], 60, loop_dir
        )
        result["steps"].append(start_record)
        if start_code != 0:
            result["steps"].extend(
                {"name": step.name, "status": "SKIP", "reason": "container start failed", "duration_seconds": 0.0}
                for step in steps
            )
            failed = True
            return result
        setup_command = workspace_copy_command(args, name)
        setup_code, _setup_output, setup_record = run_lifecycle(
            "workspace-copy", setup_command, args.timeout_seconds, loop_dir
        )
        result["steps"].append(setup_record)
        if setup_code != 0:
            result["steps"].extend(
                {"name": step.name, "status": "SKIP", "reason": "workspace copy failed", "duration_seconds": 0.0}
                for step in steps
            )
            failed = True
            return result
        versions_code, versions_output, versions_record = run_lifecycle(
            "tool-versions",
            docker_exec_base(args.runtime, name, args.source_date_epoch) + [
                "bash", "-lc", tool_versions_command(),
            ],
            60, loop_dir,
        )
        result["steps"].append(versions_record)
        result["versions"] = versions_output.strip()
        if versions_code != 0:
            result["steps"].extend(
                {"name": step.name, "status": "SKIP", "reason": "tool version probe failed", "duration_seconds": 0.0}
                for step in steps
            )
            failed = True
            return result
        wayland_ready = False
        for position, step in enumerate(steps, 1):
            started = time.monotonic()
            timed_out = False
            include_wayland = wayland_ready or step.name == "headless-sway-start"
            try:
                completed = run_process(
                    docker_exec_base(
                        args.runtime, name, args.source_date_epoch,
                        include_wayland=include_wayland,
                    ) + list(step.command),
                    args.timeout_seconds,
                )
                code, output = completed.returncode, completed.stdout
            except subprocess.TimeoutExpired as error:
                timed_out = True
                code = -1
                raw = error.stdout or ""
                output = raw.decode(errors="replace") if isinstance(raw, bytes) else raw
            except OSError as error:
                code, output = -1, str(error)
            duration = time.monotonic() - started
            log_name = f"{position:02d}-{step.name}.log"
            (loop_dir / log_name).write_text(output, encoding="utf-8", errors="replace")
            if timed_out:
                status, reason = "INFRASTRUCTURE", f"timed out after {args.timeout_seconds} seconds"
            elif code == -1:
                status, reason = "INFRASTRUCTURE", output
            elif docker_transport_failure(code, output):
                status, reason = "INFRASTRUCTURE", "Docker transport or daemon failure"
            else:
                status, reason = validate_step(step, code, output)
            result["steps"].append(
                {
                    "name": step.name, "status": status, "reason": reason,
                    "command": list(step.command), "returncode": code,
                    "duration_seconds": duration, "log": str(Path(f"loop-{index}") / log_name),
                    "output_tail": output[-4000:],
                }
            )
            if step.name == "headless-sway-start" and status == "PASS":
                wayland_ready = True
            if status in {"FAIL", "INFRASTRUCTURE"}:
                failed = True
                result["steps"].extend(
                    {
                        "name": remaining.name, "status": "SKIP",
                        "reason": f"loop stopped after {step.name}", "duration_seconds": 0.0,
                    }
                    for remaining in steps[position:]
                )
                break
    except subprocess.TimeoutExpired as error:
        failed = True
        result["steps"].append({"name": "container-lifecycle", "status": "INFRASTRUCTURE", "reason": "container lifecycle timed out", "output_tail": timeout_output(error), "duration_seconds": 0.0})
        recorded_names = {recorded["name"] for recorded in result["steps"]}
        result["steps"].extend(
            {"name": step.name, "status": "SKIP", "reason": "container lifecycle timed out", "duration_seconds": 0.0}
            for step in steps if step.name not in recorded_names
        )
    except OSError as error:
        failed = True
        result["steps"].append({"name": "container-lifecycle", "status": "INFRASTRUCTURE", "reason": str(error), "duration_seconds": 0.0})
        recorded_names = {recorded["name"] for recorded in result["steps"]}
        result["steps"].extend(
            {"name": step.name, "status": "SKIP", "reason": "container lifecycle could not continue", "duration_seconds": 0.0}
            for step in steps if step.name not in recorded_names
        )
    finally:
        if created:
            evidence_code, evidence_output, evidence_record = run_lifecycle(
                "compositor-evidence-stage",
                docker_exec_base(
                    args.runtime, name, args.source_date_epoch, include_wayland=True
                ) + [
                    "bash", "-lc", evidence_stage_command(),
                ],
                60, loop_dir,
            )
            if evidence_code == 0:
                evidence_status, evidence_reason = validate_step(
                    Step("compositor-evidence-stage", ("bash",), "evidence", required=False),
                    evidence_code,
                    evidence_output,
                )
                evidence_record["status"] = evidence_status
                evidence_record["reason"] = evidence_reason
            if evidence_record["status"] == "INFRASTRUCTURE":
                failed = True
            result["steps"].append(evidence_record)
            export_code, export_output, export_record = run_lifecycle(
                "container-artifact-export",
                [args.runtime, "cp", f"{name}:/artifacts/.", str(loop_dir)],
                120, loop_dir,
            )
            result["steps"].append(export_record)
            result["artifact_export"] = {
                "ok": export_code == 0,
                "detail": export_output.strip(),
            }
            if export_code != 0:
                failed = True
        if created and not (failed and args.keep_failed_container):
            cleanup_started = time.monotonic()
            ok, detail = exact_cleanup(args.runtime, name, token)
            cleanup_duration = time.monotonic() - cleanup_started
            cleanup_log = "lifecycle-container-cleanup.log"
            (loop_dir / cleanup_log).write_text(detail, encoding="utf-8", errors="replace")
            cleanup_record = {
                "name": "container-cleanup",
                "status": "PASS" if ok else "INFRASTRUCTURE",
                "reason": detail,
                "command": [args.runtime, "rm", "--force", "--volumes", name],
                "duration_seconds": cleanup_duration,
                "log": str(Path(f"loop-{index}") / cleanup_log),
                "output_tail": detail[-4000:],
                "kind": "lifecycle",
            }
            result["steps"].append(cleanup_record)
            result["cleanup"] = {"attempted": True, "ok": ok, "detail": detail}
            if not ok:
                failed = True
        elif created:
            result["cleanup"] = {"attempted": False, "ok": False, "detail": "failed container preserved by request"}
        loop_summary = {"PASS": 0, "FAIL": 0, "SKIP": 0, "UNAVAILABLE": 0, "INFRASTRUCTURE": 0}
        for recorded in result["steps"]:
            loop_summary[recorded["status"]] = loop_summary.get(recorded["status"], 0) + 1
        result["summary"] = loop_summary
        result["finished_at"] = utc_now()
    return result


def update_summary(report: dict[str, object]) -> None:
    summary = {"PASS": 0, "FAIL": 0, "SKIP": 0, "UNAVAILABLE": 0, "INFRASTRUCTURE": 0}
    for loop in report["loops"]:
        for step in loop["steps"]:
            summary[step["status"]] = summary.get(step["status"], 0) + 1
    report["summary"] = summary
    if summary["FAIL"]:
        report["status"] = "FAIL"
    elif summary["INFRASTRUCTURE"]:
        report["status"] = "INFRASTRUCTURE"
    else:
        report["status"] = "PASS"


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        validate_args(args)
    except ValueError as error:
        parser().error(str(error))
    steps = schedule(workspace_version(args.repo_root))
    if args.list_schedule:
        for step in steps:
            print(f"{step.name} [{step.validator}]: {step.command[0]}")
        return 0
    if args.dry_run:
        token = "DRYRUN00000000000000000000000000"
        loop_dir = args.artifacts_dir / "loop-1"
        print("Docker creates a Linux container, not a VM.")
        print("CREATE:", json.dumps(create_command(args, "chibipop-linux-regression-dryrun-1", token, loop_dir)))
        for step in steps:
            print("STEP:", step.name, json.dumps(list(step.command)))
        return 0

    report = empty_report(args)
    args.source_date_epoch = str(report["metadata"].get("source_date_epoch") or "0")
    args.artifacts_dir.mkdir(parents=True, exist_ok=True)
    write_outputs(report, args.artifacts_dir)
    runtime_path = shutil.which(args.runtime)
    if runtime_path is None:
        report["preflight"] = {"runtime_cli": "MISSING", "daemon": "NOT_CHECKED", "error": f"runtime CLI not found: {args.runtime}"}
        mark_preflight_infrastructure(report)
        report["finished_at"] = utc_now()
        write_outputs(report, args.artifacts_dir)
        return 2
    info = runtime_daemon_probe(args.runtime)
    if info.returncode != 0:
        report["preflight"] = {"runtime_cli": "FOUND", "daemon": "UNREACHABLE", "error": info.stdout.strip()}
        mark_preflight_infrastructure(report)
        report["finished_at"] = utc_now()
        write_outputs(report, args.artifacts_dir)
        return 2
    report["preflight"] = {"runtime_cli": "FOUND", "daemon": "REACHABLE", "server_version": info.stdout.strip()}
    dockerfile = args.repo_root / "scripts" / "docker" / "linux-regression.Dockerfile"
    if not args.skip_image_build:
        try:
            build = run_process(
                [
                    args.runtime, "build", "--platform", args.platform,
                    "--file", str(dockerfile), "--tag", args.image,
                    str(dockerfile.parent),
                ],
                args.timeout_seconds,
            )
            build_output = build.stdout
            build_error = None if build.returncode == 0 else f"image build failed with exit code {build.returncode}"
        except subprocess.TimeoutExpired as error:
            build_output = timeout_output(error)
            build_error = f"image build timed out after {args.timeout_seconds} seconds"
        except OSError as error:
            build_output = str(error)
            build_error = f"image build could not start: {error}"
        (args.artifacts_dir / "image-build.log").write_text(build_output, encoding="utf-8", errors="replace")
        if build_error is not None:
            mark_preflight_infrastructure(report, build_error)
            report["finished_at"] = utc_now()
            write_outputs(report, args.artifacts_dir)
            return 1
    try:
        inspect = run_process([args.runtime, "image", "inspect", "--format", "{{.Id}}", args.image], 30)
        if inspect.returncode == 0:
            report["metadata"]["image_id"] = inspect.stdout.strip()
            report["preflight"]["image"] = "FOUND"
        else:
            report["metadata"]["image_id"] = None
            report["preflight"]["image"] = "MISSING"
            report["preflight"]["error"] = inspect.stdout.strip() or f"image not found: {args.image}"
    except (OSError, subprocess.TimeoutExpired) as error:
        report["metadata"]["image_id"] = None
        report["preflight"]["image"] = "UNREACHABLE"
        report["preflight"]["error"] = str(error)
    if report["metadata"]["image_id"] is None:
        mark_preflight_infrastructure(report)
        report["finished_at"] = utc_now()
        write_outputs(report, args.artifacts_dir)
        return 1
    for index in range(1, args.loops + 1):
        report["loops"].append(run_loop(args, index, uuid.uuid4().hex, steps))
        update_summary(report)
        write_outputs(report, args.artifacts_dir)
    report["finished_at"] = utc_now()
    update_summary(report)
    write_outputs(report, args.artifacts_dir)
    return 1 if report["status"] in {"FAIL", "INFRASTRUCTURE"} else 0


if __name__ == "__main__":
    raise SystemExit(main())
