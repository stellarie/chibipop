# OCR performance reports

The repository provides a report-only Windows monitor for OCR lifecycle and
resource observations. It uses the committed
`crates/chibipop-windows/tests/fixtures/japanese_bgra.bin` fixture.

Run the report from the repository root:

```powershell
cargo test --release -p chibipop-windows --test ocr_performance --no-run
$test = Get-ChildItem target/release/deps/ocr_performance-*.exe |
  Sort-Object LastWriteTime | Select-Object -Last 1
pwsh -File scripts/ocr_performance_report.ps1 `
  -TestExecutablePath $test.FullName -OutputPath ocr-performance.json
```

Use `-DryRun` to inspect the two backend commands without starting OCR. The report
runs an already-built release test executable. This keeps Cargo
compilation outside the timed process sample. The report may list MeikiOCR as
`unavailable` when its Python package or model is not installed.

Run the checked-in PowerShell coverage with:

```powershell
pwsh -File scripts/tests/test_ocr_performance.ps1
```

The threshold helper is `scripts/ocr_performance_thresholds.ps1`. Tests use
synthetic values only. Production thresholds remain disabled until review.

## Schema

The top-level schema is `chibipop-ocr-performance/v1`. The report includes:

- `mode`: always `report-only`.
- `runner`: image, operating system, architecture, and build revision.
- `fixture`: ID, SHA-256, dimensions, and pixel format.
- `backends`: one result for Windows OCR and one for MeikiOCR.
- `comparability`: identity fields and the no-baseline comparison rule.
- `baseline`: optional reviewed input and per-backend comparisons.
- `thresholds`: separate disabled warning and failure threshold objects.
- `categories`: warning and failure category names.
- `product_goals`: separate 100 MiB and 200 MiB product goals.
- `privacy`: included and excluded data classes.

Each backend result contains `status`, actual backend identity, recognition
samples, phase intervals, latency aggregates, stable hashes, resource samples,
and warning or failure categories. Backend identity includes language, scale,
thread settings, model hashes, plugin hashes, and a configuration hash.
MeikiOCR identity is comparable only when safe discovery finds a referenced
model asset. Unknown model identity marks the backend `not-comparable`.

The benchmark records these phases in this order:

1. `cold-startup`: construct the backend or complete the plugin handshake.
2. `post-handshake-idle`: wait after startup before recognition.
3. `first-recognition`: run the first recognition on the fixture.
4. `repeated-identical-recognition`: run seven identical fixture requests.
5. `uncached-recognition`: run a fixture-derived 2x request.
6. `post-activity-steady-state`: wait after recognition activity.

Recognition reports include per-call latency, median, p95, peak, text SHA-256,
and canonical geometry SHA-256. The geometry string uses line and word indexes
and physical rectangle fields. Repeated hash stability is reported separately.

The resource sampler emits `chibipop-ocr-resources/v1`. Each sample records the
parent, every descendant, and a process-tree total. Each row includes working
set, private bytes, cumulative CPU, normalized CPU, threads, handles, phase,
runner identity, and backend identity. It also emits phase and role aggregates
with median, p95, and peak values. Executable paths reduce to non-personal
names. The report records child exit code and explicit timeout state.

Phase events use UTC timestamps in an append-only file. Resource samples use
those intervals when a phase is active. The final report retains every phase
event even when a phase is shorter than the sample interval. The report runner
passes a post-action hold of at least two sample intervals. The sampler reports
required, observed, and missing phase rows. Missing resource coverage fails the
run instead of producing a false-green report.

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

The build revision is recorded but does not block comparison. Pass an optional
reviewed report with `-BaselinePath baseline.json`. A different build revision
can compare when every listed identity field matches. A missing or invalid
baseline is an input failure.

The first stage defines no numeric baseline. It records observations and keeps
all performance categories report-only. Warning and failure threshold objects
use `enabled: false` and `value: null`. Generic delta evaluation returns
`disabled` until reviewed values exist. A later threshold change needs repeated
matching reports, reviewed values, and an explicit policy update.

Warning categories include `working-set-observation`,
`private-bytes-observation`, `latency-observation`, `cpu-observation`,
`thread-growth-observation`, `handle-growth-observation`,
`unstable-text-hash`, `unstable-geometry-hash`, and `identity-incomplete`.

Failure categories describe lifecycle or execution problems. They include
`backend-unavailable`, `launch-error`, `child-failure`, `timeout`,
`benchmark-command`, `benchmark-report-missing`, `benchmark-report-failed`,
`resource-report-missing`, `recognition-error`, `cleanup-survivor`,
`cleanup-identity-unverified`, `cleanup-identity-mismatch`,
`phase-resource-incomplete`, `benchmark-phase-incomplete`, and `baseline-input`.
Backend unavailability is
report-only. Other categories fail the report command. A survivor is a hard
failure after JSON output.

## Product goals

The goals are separate from baseline comparison:

- Windows OCR working set: 100 MiB.
- MeikiOCR working set: 200 MiB.

The report labels both goals `not-enforced`. A measured value does not claim
that a goal passed or failed.

## Privacy and cleanup

Artifacts include only the committed fixture identity, OCR text, canonical
hashes, timing, and process-tree metrics. They exclude personal configs,
dictionary data, screenshots, Anki data, and absolute personal paths.

The sampler tracks only the process that it launches and descendants observed
in its process table. It records each process start time. Cleanup validates the
PID and start time before stopping or checking a process. A reused PID is not
stopped. An identity that cannot be validated remains a hard cleanup failure.
The sampler returns a nonzero exit code for child failure, timeout, or cleanup
survivors.

The CI job uploads the final report as a diagnostic artifact. Artifact upload
does not change report-only classification. A sampler cleanup failure remains
visible as a failed job.
