# OCR performance reports

The repository provides a report-only Rust monitor for OCR lifecycle and
resource observations. Shared support lives under `crates/ocr-performance/`.
The monitor uses the committed
`crates/chibipop-windows/tests/fixtures/japanese_bgra.bin` fixture.

## Run a report

Build and run the native reporter for the target platform:

```powershell
cargo test --release -p chibipop-windows --test ocr_performance --no-run
cargo build --release -p chibipop-windows --example ocr_performance_windows
$env:CHIBIPOP_OCR_PERF_OUTPUT = "ocr-performance-windows.json"
cargo run --release -p chibipop-windows --example ocr_performance_windows
```

```bash
cargo test --release -p chibipop-linux --test ocr_performance --no-run
cargo build --release -p chibipop-linux --example ocr_performance_linux
CHIBIPOP_OCR_PERF_OUTPUT=ocr-performance-linux.json \
  cargo run --release -p chibipop-linux --example ocr_performance_linux
```

The Windows reporter measures Windows OCR and an optional MeikiOCR plugin.
Set `CHIBIPOP_OCR_PERF_PLUGIN` to the plugin directory to enable that run.
The Linux reporter measures the bundled MeikiOCR engine.

The reporter starts one child for each backend. The child runs the benchmark.
The parent samples the child process tree with native Rust APIs.
The parent removes its temporary files before writing the report.
Temporary-directory removal is a hard failure.

Optional environment variables control the report:

- `CHIBIPOP_OCR_PERF_OUTPUT` sets the report path.
- `CHIBIPOP_OCR_PERF_BASELINE` supplies a reviewed matching report.
- `CHIBIPOP_OCR_PERF_PLUGIN` selects a Windows MeikiOCR plugin directory.
- `CHIBIPOP_OCR_PERF_FIXTURE_PATH` selects the checked-in fixture path.
- `CHIBIPOP_OCR_PERF_DURATION_SECONDS` sets the resource sample duration.
- `CHIBIPOP_OCR_PERF_SAMPLE_MS` sets the resource sample interval.
- `CHIBIPOP_OCR_PERF_IDLE_MS` sets the post-start and post-activity idle hold.
- `CHIBIPOP_OCR_PERF_PHASE_HOLD_MS` sets the minimum phase hold.

Resource reports serialize `timeout_milliseconds` and lossless
`timeout_seconds` values. Subsecond durations are not truncated.
The phase hold must be at least `max(4 * sample interval, 1000 ms)`.

The ignored wrapper test runs the child benchmark directly:

```bash
cargo test -p chibipop-windows --test ocr_performance -- --ignored --nocapture
cargo test -p chibipop-linux --test ocr_performance -- --ignored --nocapture
```

After the `CI` workflow completes successfully for a `pull_request`,
`.github/workflows/ocr-performance-pr-summary.yml` runs from its
default-branch definition. It downloads only the named Windows artifact from
the triggering run and updates one marker-based bot comment with backend
status, six-phase coverage, key aggregates, cleanup survivors, disabled
report-only thresholds, the commit, and workflow run and full artifact links.

The summary workflow never checks out or executes pull-request code. It uses
only `actions: read` and `issues: write`. Forks, read-only tokens, missing or
ambiguous artifacts, malformed reports, and API failures skip optional comment
delivery without changing the CI measurement result. The full JSON report
remains the diagnostic artifact.

The comment includes a fenced plain-text visualization. It uses deterministic
proportional bars with one shared maximum for each working-set, private-bytes,
and repeated-identical p95 latency metric. It shows numeric values beside each
bar. Resource bars use the maximum valid total across all observed phases.
It labels null, zero, and unavailable values safely, and omits harness-only
resource values for unavailable backends.

## Schema

The top-level schema is `chibipop-ocr-performance/v1`.
The resource schema is `chibipop-ocr-resources/v1`.
The report includes:

- `schema`: always `chibipop-ocr-performance/v1`.
- `generated_at`: the UTC timestamp of the report.
- `mode`: always `report-only`.
- `measurement_target`: the measurement layer, `rust-native`.
- `sample_milliseconds`: the resource sample interval.
- `phase_hold_milliseconds`: the minimum hold inside each phase.
- `runner`: image, operating system, architecture, and build revision.
- `fixture`: ID, SHA-256, dimensions, and pixel format.
- `backends`: one result for each requested platform backend.
- `comparability`: identity fields and the matching rule.
- `baseline`: optional reviewed input and per-backend comparisons.
- `thresholds`: separate disabled warning and failure threshold objects.
- `categories`: warning and failure category names.
- `failure_categories`: the failures that decide the exit code.
- `product_goals`: separate 100 MiB and 200 MiB product goals.
- `privacy`: included and excluded data classes.

Each backend result contains `status`, actual backend identity, recognition
samples, phase intervals, latency aggregates, stable hashes, resource samples,
and warning or failure categories. Backend identity includes language, scale,
thread settings, model hashes, plugin hashes, and a configuration hash.
Native Linux and Windows identities report both benchmark scales, `1` and `2`.
Unknown model identity marks a MeikiOCR backend `not-comparable`.

The benchmark records these phases in this order:

1. `cold-startup`: construct the backend or complete the plugin handshake.
2. `post-handshake-idle`: wait after startup before recognition.
3. `first-recognition`: run the first recognition on the fixture.
4. `repeated-identical-recognition`: run seven identical fixture requests.
5. `uncached-recognition`: run a fixture-derived 2x request.
6. `post-activity-steady-state`: wait after recognition activity.

Recognition reports include per-call latency, median, p95, peak, OCR text,
text SHA-256, and canonical geometry SHA-256.
Repeated hash stability is reported separately.
A fixture text mismatch is a benchmark failure.

The geometry string uses line and word indexes and physical rectangle fields.
The monitor does not store screenshots or absolute paths.
Quoted Windows, Unix, and UNC paths are fully redacted, including spaces.

Resource samples record the parent, every observed descendant, and a
process-tree total. Each row includes working set, private bytes, cumulative
CPU, normalized CPU, threads, handles, phase, runner identity, and backend
identity. It also emits phase and role aggregates with median, p95, and peak.
The monitor reports child exit code and explicit timeout state.

`handles` is a Win32 handle count on Windows.
On Linux it counts the open file descriptors in `/proc/<pid>/fd`.
The two platforms therefore report the same field with different units.

Phase events use UTC timestamps in an append-only file.
Resource samples use those intervals when a phase is active.
The reporter keeps each phase for at least four sample intervals and 1000 ms.
The runner rejects a shorter phase hold before launching a child.
Missing, empty, or unreadable phase sidecars produce hard failures.
Missing resource coverage also produces a hard failure.

## Comparability and categories

Reports compare only when every identity field matches:

- runner image
- operating system
- architecture
- backend ID and actual version
- backend language
- backend thread settings
- backend model hashes
- backend plugin hashes
- backend configuration hash
- backend model asset count and completeness
- fixture SHA-256
- backend scale list
- repeated-identical text SHA-256 and geometry SHA-256
- repeated-identical text and geometry stability

The build revision is recorded but does not block comparison.
A different build revision can compare when every listed identity field matches.
Linux reports compare only to matching Linux runner identity.
A missing or invalid baseline is an input failure.
The baseline schema requires runner, fixture, and backend objects.
It also requires process-resource objects and stable-hash fields.
Malformed baseline data reports `baseline-input`.
Stable output hashes must exist in both reports and must match before a
comparison is comparable.
Unstable text or geometry hashes force `not-comparable`.

The first stage defines no numeric baseline.
All performance categories remain report-only.
Warning and failure threshold objects use `enabled: false` and `value: null`.
The disabled threshold list covers working set, private bytes, latency p95,
CPU, thread growth, handle growth, and cleanup survivors.

Warning categories include `working-set-observation`,
`private-bytes-observation`, `latency-observation`, `cpu-observation`,
`thread-growth-observation`, `handle-growth-observation`,
`unstable-text-hash`, `unstable-geometry-hash`, and `identity-incomplete`.

Failure categories include `backend-unavailable`,
`required-backend-unavailable`, `launch-error`,
`child-failure`, `timeout`, `benchmark-command`,
`benchmark-report-missing`, `benchmark-report-failed`,
`resource-report-missing`, `resource-metric-missing`, `recognition-error`,
`cleanup-survivor`, `cleanup-identity-unverified`,
`cleanup-identity-mismatch`, `phase-resource-incomplete`,
`benchmark-phase-incomplete`, `baseline-input`,
`baseline-output-mismatch`, `cleanup-signal`, `identity-sidecar-write`,
`identity-sidecar-missing`, `phase-sidecar-write`, `phase-sidecar-read`,
`phase-sidecar-missing`, `fixture-text-mismatch`, and
`report-workspace-cleanup`.

A backend plan carries `required`. The flag decides what an unavailable
backend costs.

- `required: true` adds `required-backend-unavailable` and fails the command.
  A green run then means the backend was measured.
- `required: false` records `backend-unavailable` alone and keeps exit code 0.
  Use it for a backend that a runner cannot provide.

The Windows report requires both backends.
Its runner must install the Japanese OCR language pack and set
`CHIBIPOP_OCR_PERF_PLUGIN`. Without them the report fails instead of
reporting harness overhead as a measurement.

Other categories fail the report command.
A cleanup survivor remains a hard failure after JSON output.

## Product goals

The goals stay separate from baseline comparison:

- Windows OCR working set: 100 MiB.
- MeikiOCR working set: 200 MiB.

The report labels both goals `not-enforced`.
A measured value does not claim that a goal passed or failed.

## Privacy and cleanup

Artifacts include only the committed fixture identity, OCR text, canonical
hashes, timing, and process-tree metrics.
They exclude personal configs, dictionary data, screenshots, Anki data, and
absolute personal paths.

The sampler anchors the spawned root PID and start identity immediately.
Cleanup discovers the current descendant tree again before stopping processes.
It records each process start identity and validates every candidate.
A reused PID is not stopped.
An identity that cannot be validated remains a hard cleanup failure.
Enumeration errors cannot become an empty successful report.
Totals report null CPU rates until every process row has a rate.
Windows anchors the root creation time from the retained child process handle.
Linux signals the recorded process group when the root is absent, after identity
validation. It skips that signal for a mismatched or unanchored root.
Non-ESRCH group signal errors remain cleanup failures.
Linux reads root start ticks before its first child wait, so the unreaped PID
cannot be reused before identity capture.
Identity-sidecar write and read failures remain explicit failure categories.
The sampler returns a nonzero exit code for child failure, timeout, or cleanup
survivors.

Windows uses ToolHelp, ProcessStatus, and Threading APIs.
Linux uses `/proc`, process groups, standard library APIs, and existing `nix`.
The CI jobs upload the final report as diagnostic artifacts.
Artifact upload does not change report-only classification.
