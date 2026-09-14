//! Shared OCR reports.

use anyhow::{bail, Context, Result};
use chibipop::text::layout::{OcrLine, OcrWord};
use chibipop::text::OcrEngine;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
mod process_windows;
#[cfg(unix)]
mod process_linux;

pub const FIXTURE_WIDTH: i32 = 400;
pub const FIXTURE_HEIGHT: i32 = 120;
pub const FIXTURE_ID: &str = "japanese_bgra.bin";
pub const FIXTURE_SHA256: &str =
    "e204bfc27c9c333f792a04040fca1100ebb788b7b199a57cb0a92afaa64250e1";
pub const FIXTURE_EXPECTED: char = '昨';
pub const REPEATED_COUNT: usize = 7;
pub const PHASES: [&str; 6] = [
    "cold-startup",
    "post-handshake-idle",
    "first-recognition",
    "repeated-identical-recognition",
    "uncached-recognition",
    "post-activity-steady-state",
];

const DEFAULT_IDLE_MILLISECONDS: u64 = 300;
const MAX_IDLE_MILLISECONDS: u64 = 5_000;
const MAX_PHASE_HOLD_MILLISECONDS: u64 = 60_000;

#[derive(Clone)]
pub struct Fixture {
    pub pixels: Vec<u8>,
    pub metadata: Value,
}

pub fn load_fixture(path: &Path) -> Result<Fixture> {
    let pixels = std::fs::read(path).with_context(|| format!("reading {}", FIXTURE_ID))?;
    let expected_len = usize::try_from(FIXTURE_WIDTH)
        .and_then(|width| usize::try_from(FIXTURE_HEIGHT).map(|height| width * height * 4))
        .context("computing fixture size")?;
    if pixels.len() != expected_len {
        bail!("{FIXTURE_ID} has {} bytes, expected {expected_len}", pixels.len());
    }
    let sha256 = sha256_hex(&pixels);
    if sha256 != FIXTURE_SHA256 {
        bail!("{FIXTURE_ID} has an unexpected SHA-256");
    }
    let metadata = json!({
        "id": FIXTURE_ID,
        "sha256": sha256,
        "width": FIXTURE_WIDTH,
        "height": FIXTURE_HEIGHT,
        "pixel_format": "bgra8",
        "expected_character": FIXTURE_EXPECTED.to_string(),
    });
    Ok(Fixture { pixels, metadata })
}

pub struct BenchmarkOptions {
    pub backend_id: String,
    pub version: String,
    pub language: String,
    pub identity: Value,
    pub fixture: Value,
    pub phase_file: Option<PathBuf>,
    pub identity_file: Option<PathBuf>,
}

pub fn write_child_report(report: &Value) -> Result<()> {
    if let Some(path) = std::env::var_os("CHIBIPOP_OCR_PERF_REPORT") {
        let path = PathBuf::from(path);
        std::fs::write(&path, serde_json::to_vec_pretty(report)?).with_context(|| {
            format!("writing OCR performance report {}", redact_path(&path))
        })?;
    }
    println!("{report}");
    Ok(())
}

struct Benchmark {
    backend_id: String,
    version: String,
    language: String,
    fixture: Value,
    identity: Value,
    status: String,
    origin: Instant,
    idle: Duration,
    phase_hold: Duration,
    pixels: Vec<u8>,
    phase_file: Option<PathBuf>,
    identity_file: Option<PathBuf>,
    identity_error: Option<String>,
    phase_error: Option<String>,
    phases: Vec<Value>,
    samples: Vec<Sample>,
    warnings: Vec<&'static str>,
    failure_categories: Vec<&'static str>,
}

impl Benchmark {
    fn new(options: BenchmarkOptions, pixels: Vec<u8>) -> Self {
        let mut benchmark = Self {
            backend_id: options.backend_id,
            version: options.version,
            language: options.language,
            fixture: options.fixture,
            identity: options.identity,
            status: "starting".to_string(),
            origin: Instant::now(),
            idle: idle_duration(),
            phase_hold: phase_hold_duration(),
            pixels,
            phase_file: options.phase_file,
            identity_file: options.identity_file,
            identity_error: None,
            phase_error: None,
            phases: Vec::new(),
            samples: Vec::new(),
            warnings: Vec::new(),
            failure_categories: Vec::new(),
        };
        benchmark.write_identity();
        benchmark
    }

    fn set_version(&mut self, version: String) {
        self.version = version;
        self.write_identity();
    }

    fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
        self.write_identity();
    }

    fn phase<T, F>(&mut self, name: &str, action: F) -> Result<T>
    where
        F: FnOnce(&mut Self) -> Result<T>,
    {
        let started = self.origin.elapsed();
        let started_at = unix_millis();
        self.write_phase_event("start", name, started_at);
        let result = action(self);
        if !self.phase_hold.is_zero() {
            std::thread::sleep(self.phase_hold);
        }
        let ended = self.origin.elapsed();
        let ended_at = unix_millis();
        self.write_phase_event("end", name, ended_at);
        self.phases.push(json!({
            "name": name,
            "started_ms": duration_ms(started),
            "ended_ms": duration_ms(ended),
            "duration_ms": duration_ms(ended.saturating_sub(started)),
            "started_unix_ms": started_at,
            "ended_unix_ms": ended_at,
        }));
        if let Some(error) = &self.phase_error {
            return Err(anyhow::anyhow!(error.clone()));
        }
        result
    }

    fn write_phase_event(&mut self, event: &str, name: &str, at_unix_ms: u64) {
        if let Some(path) = &self.phase_file {
            let result = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| writeln!(file, "{event}\t{at_unix_ms}\t{name}"));
            if let Err(error) = result {
                self.phase_error.get_or_insert_with(|| {
                    format!(
                        "writing phase sidecar {} failed: {error}",
                        redact_path(path)
                    )
                });
            }
        }
    }

    fn write_identity(&mut self) {
        let Some(path) = self.identity_file.clone() else {
            return;
        };
        let value = match serde_json::to_vec(&self.backend_value()) {
            Ok(value) => value,
            Err(error) => {
                self.identity_error = Some(format!(
                    "serializing identity sidecar {} failed: {error}",
                    redact_path(&path)
                ));
                return;
            }
        };
        if let Err(error) = std::fs::write(&path, value) {
            self.identity_error = Some(format!(
                "writing identity sidecar {} failed: {error}",
                redact_path(&path)
            ));
        }
    }

    fn backend_value(&self) -> Value {
        let mut backend = json!({
            "id": self.backend_id,
            "version": self.version,
            "language": self.language,
            "geometry": true,
            "status": self.status,
        });
        if let (Some(destination), Some(source)) =
            (backend.as_object_mut(), self.identity.as_object())
        {
            for (key, value) in source {
                destination.insert(key.clone(), value.clone());
            }
        }
        backend
    }

    fn run_recognition_phases(&mut self, engine: &dyn OcrEngine) -> Value {
        let result = (|| {
            let idle = self.idle;
            self.phase("post-handshake-idle", |_| {
                std::thread::sleep(idle);
                Ok(())
            })?;
            self.phase("first-recognition", |benchmark| {
                benchmark.record(engine, "first-recognition", 1)
            })?;
            self.phase("repeated-identical-recognition", |benchmark| {
                for _ in 0..REPEATED_COUNT {
                    benchmark.record(engine, "repeated-identical-recognition", 1)?;
                }
                Ok(())
            })?;
            self.phase("uncached-recognition", |benchmark| {
                benchmark.record(engine, "uncached-recognition", 2)
            })?;
            self.phase("post-activity-steady-state", |_| {
                std::thread::sleep(idle);
                Ok(())
            })?;
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = result {
            self.set_status("failed");
            self.failure_categories.push("recognition-error");
            return self.report("failed", Some(error.to_string()));
        }
        self.set_status("available");
        if self.warnings.contains(&"fixture-text-mismatch") {
            self.set_status("failed");
            self.failure_categories.push("fixture-text-mismatch");
            return self.report("failed", None);
        }
        self.report("available", None)
    }

    fn record(&mut self, engine: &dyn OcrEngine, phase: &'static str, scale: i32) -> Result<()> {
        let (pixels, width, height) = if scale == 1 {
            (self.pixels.clone(), FIXTURE_WIDTH, FIXTURE_HEIGHT)
        } else {
            let (pixels, width, height) = chibipop::text::source::upscale_by(
                &self.pixels,
                FIXTURE_WIDTH,
                FIXTURE_HEIGHT,
                scale,
            );
            (pixels, width, height)
        };
        let started = Instant::now();
        let lines = engine.recognise(&pixels, width, height)?;
        let elapsed = started.elapsed();
        validate_lines(&lines)?;
        let text = canonical_text(&lines);
        if !text.contains(FIXTURE_EXPECTED) && !self.warnings.contains(&"fixture-text-mismatch") {
            self.warnings.push("fixture-text-mismatch");
        }
        let geometry = canonical_geometry(&lines);
        let text_sha256 = sha256_hex(text.as_bytes());
        let geometry_sha256 = sha256_hex(geometry.as_bytes());
        self.samples.push(Sample {
            phase,
            scale,
            latency_ms: elapsed.as_secs_f64() * 1000.0,
            text,
            text_sha256,
            geometry_sha256,
            word_count: lines.iter().map(|line| line.words.len()).sum(),
        });
        Ok(())
    }

    fn report(&self, status: &str, reason: Option<String>) -> Value {
        let repeated = self
            .samples
            .iter()
            .filter(|sample| sample.phase == "repeated-identical-recognition")
            .collect::<Vec<_>>();
        let stable = stable_hashes(&repeated);
        let mut warnings = self.warnings.to_vec();
        let text_stable = stable
            .get("repeated_identical")
            .and_then(|value| value.get("text_stable"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let geometry_stable = stable
            .get("repeated_identical")
            .and_then(|value| value.get("geometry_stable"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !repeated.is_empty() && !text_stable && !warnings.contains(&"unstable-text-hash") {
            warnings.push("unstable-text-hash");
        }
        if !repeated.is_empty()
            && !geometry_stable
            && !warnings.contains(&"unstable-geometry-hash")
        {
            warnings.push("unstable-geometry-hash");
        }
        let mut failures = self.failure_categories.to_vec();
        if self.warnings.contains(&"fixture-text-mismatch") {
            failures.push("fixture-text-mismatch");
        }
        if self.identity_error.is_some() {
            failures.push("identity-sidecar-write");
        }
        if self.phase_error.is_some() {
            failures.push("phase-sidecar-write");
        }
        let mut report = json!({
            "status": status,
            "backend": self.backend_value(),
            "fixture": self.fixture,
            "phases": self.phases,
            "recognitions": self.samples.iter().map(Sample::json).collect::<Vec<_>>(),
            "latency": latency_aggregates(&self.samples),
            "stable_hashes": stable,
            "warning_categories": warnings,
            "failure_categories": failures,
            "identity_sidecar_error": self
                .identity_error
                .as_deref()
                .map(redact_reason),
        });
        if let Some(reason) = reason {
            report["reason"] = Value::String(redact_reason(&reason));
        }
        report
    }
}

struct Sample {
    phase: &'static str,
    scale: i32,
    latency_ms: f64,
    text: String,
    text_sha256: String,
    geometry_sha256: String,
    word_count: usize,
}

impl Sample {
    fn json(&self) -> Value {
        json!({
            "phase": self.phase,
            "scale": self.scale,
            "latency_ms": self.latency_ms,
            "text": self.text,
            "text_sha256": self.text_sha256,
            "geometry_sha256": self.geometry_sha256,
            "word_count": self.word_count,
        })
    }
}

pub fn run_benchmark_with_fixture<F>(
    mut options: BenchmarkOptions,
    fixture: &Fixture,
    create: F,
) -> Value
where
    F: FnOnce() -> Result<(Box<dyn OcrEngine>, String)>,
{
    options.fixture = fixture.metadata.clone();
    let mut benchmark = Benchmark::new(options, fixture.pixels.clone());
    let result = benchmark.phase("cold-startup", |_| create());
    match result {
        Ok((engine, version)) => {
            benchmark.set_version(version);
            benchmark.run_recognition_phases(&*engine)
        }
        Err(error) => {
            let status = if benchmark.phase_error.is_some() {
                "failed"
            } else {
                "unavailable"
            };
            benchmark.set_status(status);
            if status == "unavailable" {
                benchmark.failure_categories.push("backend-unavailable");
            }
            benchmark.report(status, Some(error.to_string()))
        }
    }
}

#[derive(Clone)]
pub struct ProcessConfig {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub duration: Duration,
    pub sample_interval: Duration,
    pub phase_file: PathBuf,
    pub identity_file: PathBuf,
    pub required_phases: Vec<String>,
    pub backend: Value,
    pub runner: Value,
    pub fixture_sha256: String,
}

pub struct BackendPlan {
    pub id: String,
    pub child_backend: String,
    pub fallback_identity: Value,
    pub environment: BTreeMap<String, String>,
}

pub struct ReportOptions {
    pub fixture_path: PathBuf,
    pub child_program: PathBuf,
    pub backends: Vec<BackendPlan>,
    pub output_path: PathBuf,
    pub baseline_path: Option<PathBuf>,
    pub duration: Duration,
    pub sample_interval: Duration,
    pub idle: Duration,
    pub phase_hold: Duration,
    pub runner: Value,
}

fn temporary_report_directory() -> Result<PathBuf> {
    let name = format!(
        "chibipop-ocr-performance-{}-{}",
        std::process::id(),
        unix_millis()
    );
    let path = std::env::temp_dir().join(name);
    std::fs::create_dir(&path).with_context(|| {
        format!("creating OCR report workspace {}", redact_path(&path))
    })?;
    Ok(path)
}

fn empty_resource_report(config: &ProcessConfig, reason: &str) -> Value {
    json!({
        "schema": "chibipop-ocr-resources/v1",
        "started_at": system_time_string(SystemTime::now()),
        "sampled_until": system_time_string(SystemTime::now()),
        "root_pid": 0,
        "child_exit_code": Value::Null,
        "child_exited": false,
        "timed_out": false,
        "timeout_seconds": config.duration.as_secs_f64(),
        "timeout_milliseconds": config.duration.as_millis(),
        "failure_categories": ["launch-error", "resource-report-missing"],
        "reason": redact_reason(reason),
        "runner": config.runner,
        "backend": config.backend,
        "fixture_sha256": config.fixture_sha256,
        "command": {
            "file": redact_path(&config.program),
            "arguments": config.arguments.iter().map(|arg| redact_text(arg)).collect::<Vec<_>>(),
            "duration_seconds": config.duration.as_secs_f64(),
            "duration_milliseconds": config.duration.as_millis(),
            "sample_milliseconds": config.sample_interval.as_millis(),
            "logical_processors": std::thread::available_parallelism().map_or(1, |n| n.get()),
        },
        "cleanup_stopped_process_ids": [],
        "cleanup_remaining_process_ids": [],
        "cleanup_identity_mismatch_process_ids": [],
        "cleanup_identity_unverified_process_ids": [],
        "cleanup_error": Value::Null,
        "cleanup_signal_error": Value::Null,
        "sampling_error": redact_reason(reason),
        "phase_events": [],
        "phase_coverage": {
            "applicable": false,
            "required": config.required_phases,
            "observed": [],
            "missing": config.required_phases,
            "complete": false,
        },
        "records": [],
        "aggregates": [],
    })
}

fn combine_backend_result(
    plan: &BackendPlan,
    test: Option<&Value>,
    resources: &Value,
    command_exit_code: Option<i64>,
) -> Value {
    let test_result = test.cloned().unwrap_or_else(|| {
        json!({
            "status": "failed",
            "backend": plan.fallback_identity,
            "fixture": Value::Null,
            "phases": [],
            "recognitions": [],
            "latency": Value::Null,
            "stable_hashes": Value::Null,
            "warning_categories": [],
            "failure_categories": ["benchmark-report-missing"],
        })
    });
    let backend = test_result
        .get("backend")
        .cloned()
        .unwrap_or_else(|| plan.fallback_identity.clone());
    let status = test_result.get("status").and_then(Value::as_str).unwrap_or("failed");
    let unavailable = status == "unavailable"
        || test_result
            .get("failure_categories")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("backend-unavailable"))
            });
    let mut warnings = strings_from(&test_result, "warning_categories");
    let mut failures = strings_from(&test_result, "failure_categories");
    failures.extend(strings_from(resources, "failure_categories"));
    if status == "failed" {
        failures.push("benchmark-report-failed".to_string());
    }
    if !unavailable {
        if let Some(coverage) = resources.get("phase_coverage") {
            if coverage.get("applicable") != Some(&Value::Bool(true))
                || coverage.get("complete") != Some(&Value::Bool(true))
            {
                failures.push("phase-resource-incomplete".to_string());
            }
        } else {
            failures.push("phase-resource-incomplete".to_string());
        }
        if status == "available" {
            let names = test_result
                .get("phases")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.get("name").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if PHASES.iter().any(|phase| !names.contains(phase)) {
                failures.push("benchmark-phase-incomplete".to_string());
            }
        }
    }
    if backend.get("id").and_then(Value::as_str) == Some("meikiocr")
        && backend.get("identity_complete") != Some(&Value::Bool(true))
    {
        warnings.push("identity-incomplete".to_string());
    }
    if command_exit_code.is_some_and(|code| code != 0) && !unavailable {
        failures.push("benchmark-command".to_string());
    }
    json!({
        "backend": backend,
        "status": status,
        "test": test_result,
        "resources": resources,
        "command_exit_code": command_exit_code,
        "warning_categories": unique_strings(&warnings),
        "failure_categories": unique_strings(&failures),
    })
}

fn strings_from(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn baseline_report(path: Option<&Path>) -> Result<Option<Value>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let report = read_json(path).context("baseline report is missing or invalid")?;
    validate_baseline_schema(&report)?;
    Ok(Some(report))
}

fn validate_baseline_schema(report: &Value) -> Result<()> {
    if report.get("schema").and_then(Value::as_str)
        != Some("chibipop-ocr-performance/v1")
    {
        bail!("baseline report has an invalid schema");
    }
    if report.get("mode").and_then(Value::as_str) != Some("report-only") {
        bail!("baseline report has an invalid mode");
    }
    if report.get("generated_at").and_then(Value::as_str).is_none() {
        bail!("baseline report has no generated timestamp");
    }
    if report.get("measurement_target").and_then(Value::as_str)
        != Some("rust-native")
    {
        bail!("baseline measurement target is invalid");
    }
    for field in ["sample_milliseconds", "phase_hold_milliseconds"] {
        if report.get(field).and_then(Value::as_u64).is_none() {
            bail!("baseline field {field} is invalid");
        }
    }
    let runner = report
        .get("runner")
        .and_then(Value::as_object)
        .context("baseline runner must be an object")?;
    for field in ["image", "os", "architecture"] {
        if runner.get(field).and_then(Value::as_str).is_none() {
            bail!("baseline runner field {field} is invalid");
        }
    }
    if runner.get("build_revision").is_some_and(|value| {
        !value.is_null() && value.as_str().is_none()
    }) {
        bail!("baseline runner build_revision is invalid");
    }
    validate_baseline_fixture(report.get("fixture"))?;
    let backends = report
        .get("backends")
        .and_then(Value::as_array)
        .filter(|backends| !backends.is_empty())
        .context("baseline report has no backends")?;
    for (index, result) in backends.iter().enumerate() {
        validate_baseline_backend(result)
            .with_context(|| format!("baseline backend {index} is invalid"))?;
    }
    for field in ["comparability", "baseline", "thresholds", "categories", "privacy"] {
        if !report.get(field).is_some_and(Value::is_object) {
            bail!("baseline field {field} is invalid");
        }
    }
    if !report.get("failure_categories").is_some_and(Value::is_array) {
        bail!("baseline failure_categories is invalid");
    }
    Ok(())
}

fn validate_baseline_fixture(fixture: Option<&Value>) -> Result<()> {
    let fixture = fixture
        .and_then(Value::as_object)
        .context("baseline fixture must be an object")?;
    for field in ["id", "sha256", "pixel_format", "expected_character"] {
        if fixture.get(field).and_then(Value::as_str).is_none() {
            bail!("baseline fixture field {field} is invalid");
        }
    }
    let sha256 = fixture
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_sha256(sha256) {
        bail!("baseline fixture SHA-256 is invalid");
    }
    for field in ["width", "height"] {
        if fixture
            .get(field)
            .and_then(Value::as_i64)
            .is_none_or(|value| value <= 0)
        {
            bail!("baseline fixture field {field} is invalid");
        }
    }
    Ok(())
}

fn validate_baseline_backend(result: &Value) -> Result<()> {
    let result_object = result
        .as_object()
        .context("baseline backend result must be an object")?;
    if !result_object
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "available" | "unavailable" | "failed"))
    {
        bail!("baseline backend result status is invalid");
    }
    validate_backend_object(result.get("backend"))?;
    let test = result
        .get("test")
        .and_then(Value::as_object)
        .context("baseline backend test must be an object")?;
    if test.get("status").and_then(Value::as_str).is_none() {
        bail!("baseline test status is invalid");
    }
    for field in ["backend", "fixture", "phases", "recognitions"] {
        if test.get(field).is_none() {
            bail!("baseline test field {field} is missing");
        }
    }
    validate_baseline_fixture(test.get("fixture"))?;
    for field in ["phases", "recognitions"] {
        if !test.get(field).is_some_and(Value::is_array) {
            bail!("baseline test field {field} is invalid");
        }
    }
    if test.get("latency").is_some_and(|value| {
        !value.is_null() && !value.is_object()
    }) {
        bail!("baseline test latency is invalid");
    }
    for field in ["warning_categories", "failure_categories"] {
        if !test.get(field).is_some_and(Value::is_array) {
            bail!("baseline test field {field} is invalid");
        }
    }
    validate_backend_object(test.get("backend"))?;
    if result
        .get("backend")
        .and_then(|backend| backend.get("id"))
        != test
            .get("backend")
            .and_then(|backend| backend.get("id"))
    {
        bail!("baseline backend IDs do not match");
    }
    validate_stable_hashes(test.get("stable_hashes"))?;
    let resources = result
        .get("resources")
        .and_then(Value::as_object)
        .context("baseline resources must be an object")?;
    if resources.get("schema").and_then(Value::as_str)
        != Some("chibipop-ocr-resources/v1")
    {
        bail!("baseline resources schema is invalid");
    }
    validate_backend_object(resources.get("backend"))?;
    if !resources.get("runner").is_some_and(Value::is_object) {
        bail!("baseline resource runner is invalid");
    }
    if resources.get("fixture_sha256").and_then(Value::as_str).is_none() {
        bail!("baseline resource fixture hash is invalid");
    }
    for field in [
        "failure_categories",
        "cleanup_stopped_process_ids",
        "cleanup_remaining_process_ids",
        "cleanup_identity_mismatch_process_ids",
        "cleanup_identity_unverified_process_ids",
        "phase_events",
        "records",
        "aggregates",
    ] {
        if !resources.get(field).is_some_and(Value::is_array) {
            bail!("baseline resources field {field} is invalid");
        }
    }
    Ok(())
}

fn validate_backend_object(value: Option<&Value>) -> Result<()> {
    let backend = value
        .and_then(Value::as_object)
        .context("baseline backend must be an object")?;
    for field in ["id", "version", "language", "status"] {
        if backend.get(field).and_then(Value::as_str).is_none() {
            bail!("baseline backend field {field} is invalid");
        }
    }
    if !backend
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "starting" | "available" | "unavailable" | "failed"))
    {
        bail!("baseline backend status is invalid");
    }
    if !backend.get("geometry").is_some_and(Value::is_boolean) {
        bail!("baseline backend geometry is invalid");
    }
    let scales = backend
        .get("scales")
        .and_then(Value::as_array)
        .filter(|scales| scales.iter().all(Value::is_i64))
        .context("baseline backend scales are invalid")?;
    if !scales.iter().any(|value| value.as_i64() == Some(1))
        || !scales.iter().any(|value| value.as_i64() == Some(2))
    {
        bail!("baseline backend scales must include 1 and 2");
    }
    if !backend
        .get("model_hashes")
        .is_some_and(|value| value.is_object() || value.is_null())
    {
        bail!("baseline backend field model_hashes is invalid");
    }
    for field in ["plugin_hashes", "thread_settings"] {
        if !backend.get(field).is_some_and(Value::is_object) {
            bail!("baseline backend field {field} is invalid");
        }
    }
    if backend.get("identity_complete").and_then(Value::as_bool).is_none() {
        bail!("baseline backend identity_complete is invalid");
    }
    if backend.get("model_asset_count").and_then(Value::as_u64).is_none() {
        bail!("baseline backend model_asset_count is invalid");
    }
    for field in ["config_sha256", "identity_incomplete_reason"] {
        if backend.get(field).is_some_and(|value| {
            !value.is_null() && value.as_str().is_none()
        }) {
            bail!("baseline backend field {field} is invalid");
        }
    }
    Ok(())
}

fn validate_stable_hashes(value: Option<&Value>) -> Result<()> {
    let hashes = value
        .and_then(Value::as_object)
        .context("baseline stable_hashes must be an object")?;
    let repeated = hashes
        .get("repeated_identical")
        .and_then(Value::as_object)
        .context("baseline repeated hashes must be an object")?;
    for field in ["text_stable", "geometry_stable"] {
        if repeated.get(field).and_then(Value::as_bool).is_none() {
            bail!("baseline stable hash field {field} is invalid");
        }
    }
    for field in ["text_sha256", "geometry_sha256"] {
        if repeated.get(field).is_some_and(|value| {
            !value.is_null() && value.as_str().is_none_or(|hash| !is_sha256(hash))
        }) {
            bail!("baseline stable hash field {field} is invalid");
        }
    }
    if repeated.get("text_stable") == Some(&Value::Bool(true))
        && repeated
            .get("text_sha256")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("baseline stable text hash is missing");
    }
    if repeated.get("geometry_stable") == Some(&Value::Bool(true))
        && repeated
            .get("geometry_sha256")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("baseline stable geometry hash is missing");
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn comparisons(
    current: &[Value],
    fixture: &Value,
    runner: &Value,
    baseline: Option<&Value>,
) -> Vec<Value> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let baseline_runner = baseline.get("runner").cloned().unwrap_or_default();
    let baseline_fixture = baseline.get("fixture").cloned().unwrap_or_default();
    current
        .iter()
        .map(|result| {
            let backend_id = result
                .get("backend")
                .and_then(|backend| backend.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let baseline_result = baseline
                .get("backends")
                .and_then(Value::as_array)
                .and_then(|values| {
                    values.iter().find(|candidate| {
                        candidate
                            .get("backend")
                            .and_then(|backend| backend.get("id"))
                            .and_then(Value::as_str)
                            == Some(backend_id)
                    })
                });
            let Some(baseline_result) = baseline_result else {
                return json!({
                    "backend_id": backend_id,
                    "status": "baseline-backend-missing",
                    "metrics": []
                });
            };
            let current_identity = identity_report(result, fixture, runner);
            let baseline_identity = identity_report(
                baseline_result,
                &baseline_fixture,
                &baseline_runner,
            );
            let comparable = compare_identity_values(&current_identity, &baseline_identity);
            let output_hashes_stable = output_hashes_stable(&current_identity)
                && output_hashes_stable(&baseline_identity);
            let output_hashes_match = compare_output_hashes(
                &current_identity,
                &baseline_identity,
            );
            let mut metrics = Vec::new();
            for metric in [
                "working_set_bytes",
                "private_bytes",
                "latency_p95_ms",
                "cpu_peak_percent",
                "threads_peak",
                "handles_peak",
                "cleanup_survivor_count",
            ] {
                let current_value = metric_value(result, metric);
                let baseline_value = metric_value(baseline_result, metric);
                let (delta, delta_percent) = match (current_value, baseline_value) {
                    (Some(current), Some(baseline)) => {
                        let delta = current - baseline;
                        let percent = (baseline != 0.0).then(|| 100.0 * delta / baseline);
                        (Some(delta), percent)
                    }
                    _ => (None, None),
                };
                let threshold = new_thresholds()
                    .get(metric)
                    .cloned()
                    .unwrap_or(Value::Null);
                metrics.push(json!({
                    "metric": metric,
                    "current": current_value,
                    "baseline": baseline_value,
                    "delta": delta,
                    "delta_percent": delta_percent,
                    "warning": threshold.get("warning").map_or("disabled", |value| {
                        threshold_state(delta_percent, value)
                    }),
                    "failure": threshold.get("failure").map_or("disabled", |value| {
                        threshold_state(delta_percent, value)
                    }),
                }));
            }
            json!({
                "backend_id": backend_id,
                "status": if comparable { "comparable" } else { "not-comparable" },
                "output_hashes_stable": output_hashes_stable,
                "output_hashes_match": output_hashes_match,
                "current_identity": current_identity,
                "baseline_identity": baseline_identity,
                "current_build_revision": runner.get("build_revision"),
                "baseline_build_revision": baseline_runner.get("build_revision"),
                "build_revision_match_required": false,
                "metrics": metrics,
            })
        })
        .collect()
}

fn identity_report(result: &Value, fixture: &Value, runner: &Value) -> Value {
    let backend = result.get("backend").cloned().unwrap_or_default();
    json!({
        "runner_image": runner.get("image"),
        "runner_os": runner.get("os"),
        "runner_architecture": runner.get("architecture"),
        "backend_id": backend.get("id"),
        "backend_version": backend.get("version"),
        "language": backend.get("language"),
        "thread_settings": backend.get("thread_settings"),
        "model_hashes": backend.get("model_hashes"),
        "plugin_hashes": backend.get("plugin_hashes"),
        "config_sha256": backend.get("config_sha256"),
        "model_asset_count": backend.get("model_asset_count"),
        "identity_complete": backend.get("identity_complete"),
        "fixture_sha256": fixture.get("sha256"),
        "scales": backend.get("scales"),
        "output_text_sha256": result
            .get("test")
            .and_then(|test| test.get("stable_hashes"))
            .and_then(|hashes| hashes.get("repeated_identical"))
            .and_then(|repeated| repeated.get("text_sha256")),
        "output_geometry_sha256": result
            .get("test")
            .and_then(|test| test.get("stable_hashes"))
            .and_then(|hashes| hashes.get("repeated_identical"))
            .and_then(|repeated| repeated.get("geometry_sha256")),
        "output_text_stable": result
            .get("test")
            .and_then(|test| test.get("stable_hashes"))
            .and_then(|hashes| hashes.get("repeated_identical"))
            .and_then(|repeated| repeated.get("text_stable")),
        "output_geometry_stable": result
            .get("test")
            .and_then(|test| test.get("stable_hashes"))
            .and_then(|hashes| hashes.get("repeated_identical"))
            .and_then(|repeated| repeated.get("geometry_stable")),
    })
}

fn compare_identity_values(current: &Value, baseline: &Value) -> bool {
    let same_fields = [
        "runner_image",
        "runner_os",
        "runner_architecture",
        "backend_id",
        "backend_version",
        "language",
        "thread_settings",
        "model_hashes",
        "plugin_hashes",
        "config_sha256",
        "model_asset_count",
        "identity_complete",
        "fixture_sha256",
        "scales",
        "output_text_sha256",
        "output_geometry_sha256",
        "output_text_stable",
        "output_geometry_stable",
    ]
    .iter()
    .all(|field| current.get(field) == baseline.get(field));
    let identity_usable = current.get("backend_id").and_then(Value::as_str) != Some("meikiocr")
        || (current.get("identity_complete") == Some(&Value::Bool(true))
            && baseline.get("identity_complete") == Some(&Value::Bool(true)));
    same_fields && identity_usable && output_hashes_stable(current)
        && output_hashes_stable(baseline)
}

fn compare_output_hashes(current: &Value, baseline: &Value) -> bool {
    if !output_hashes_stable(current) || !output_hashes_stable(baseline) {
        return false;
    }
    [
        "output_text_sha256",
        "output_geometry_sha256",
        "output_text_stable",
        "output_geometry_stable",
    ]
    .iter()
    .all(|field| current.get(field) == baseline.get(field))
}

fn output_hashes_stable(value: &Value) -> bool {
    value.get("output_text_stable") == Some(&Value::Bool(true))
        && value.get("output_geometry_stable") == Some(&Value::Bool(true))
        && value.get("output_text_sha256").and_then(Value::as_str).is_some()
        && value
            .get("output_geometry_sha256")
            .and_then(Value::as_str)
            .is_some()
}

fn metric_value(result: &Value, metric: &str) -> Option<f64> {
    if metric == "latency_p95_ms" {
        return result
            .get("test")
            .and_then(|test| test.get("latency"))
            .and_then(|latency| latency.get("repeated_identical"))
            .and_then(|stats| stats.get("p95_ms"))
            .and_then(Value::as_f64);
    }
    if metric == "cleanup_survivor_count" {
        return result
            .get("resources")
            .and_then(|resources| resources.get("cleanup_remaining_process_ids"))
            .and_then(Value::as_array)
            .map(|values| values.len() as f64);
    }
    let field = match metric {
        "working_set_bytes" => "working_set_bytes",
        "private_bytes" => "private_bytes",
        "cpu_peak_percent" => "cpu_percent",
        "threads_peak" => "threads",
        "handles_peak" => "handles",
        _ => return None,
    };
    result
        .get("resources")
        .and_then(|resources| resources.get("aggregates"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|aggregate| aggregate.get("role").and_then(Value::as_str) == Some("total"))
        .filter_map(|aggregate| aggregate.get(field))
        .filter_map(|stats| stats.get("peak").and_then(Value::as_f64))
        .max_by(f64::total_cmp)
}

fn threshold_report() -> Value {
    let thresholds = new_thresholds();
    let rows = thresholds
        .as_object()
        .into_iter()
        .flat_map(|values| values.iter())
        .map(|(metric, value)| {
            json!({
                "metric": metric,
                "threshold": value.get("warning"),
            })
        })
        .collect::<Vec<_>>();
    let failures = thresholds
        .as_object()
        .into_iter()
        .flat_map(|values| values.iter())
        .map(|(metric, value)| {
            json!({
                "metric": metric,
                "threshold": value.get("failure"),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "warning": rows,
        "failure": failures,
        "evaluation": concat!(
            "Generic delta evaluation is disabled until reviewed thresholds ",
            "provide values."
        ),
    })
}

pub fn run_report(options: &ReportOptions) -> Result<Value> {
    if options.duration.is_zero() {
        bail!("OCR report duration must be positive");
    }
    if options.sample_interval.is_zero() {
        bail!("OCR report sample interval must be positive");
    }
    let minimum_hold = minimum_phase_hold(options.sample_interval);
    if options.phase_hold < minimum_hold {
        bail!("OCR phase hold must cover four sample intervals");
    }
    let fixture = load_fixture(&options.fixture_path)?;
    let work_root = temporary_report_directory()?;
    let result = run_report_inner(options, &fixture, &work_root);
    let cleanup = std::fs::remove_dir_all(&work_root);
    let mut report = match result {
        Ok(report) => report,
        Err(error) => {
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error.context(format!(
                    "removing OCR report workspace failed: {}",
                    redact_reason(&cleanup_error.to_string())
                ))),
            };
        }
    };
    if let Err(error) = cleanup {
        if let Some(object) = report.as_object_mut() {
            if let Some(categories) = object
                .get_mut("failure_categories")
                .and_then(Value::as_array_mut)
            {
                categories.push(Value::String("report-workspace-cleanup".to_string()));
            }
            object.insert(
                "cleanup_error".to_string(),
                Value::String(redact_reason(&error.to_string())),
            );
        }
        write_report(options, &report)?;
        bail!("removing OCR report workspace failed: {}", redact_reason(&error.to_string()));
    }
    write_report(options, &report)?;
    Ok(report)
}

pub fn minimum_phase_hold(sample_interval: Duration) -> Duration {
    sample_interval
        .checked_mul(4)
        .unwrap_or(Duration::MAX)
        .max(Duration::from_secs(1))
}

fn run_report_inner(
    options: &ReportOptions,
    fixture: &Fixture,
    work_root: &Path,
) -> Result<Value> {
    let mut backend_results = Vec::new();
    let mut overall_failures = Vec::new();
    for plan in &options.backends {
        let safe_name = plan
            .id
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
            .collect::<String>();
        let test_path = work_root.join(format!("{safe_name}-test.json"));
        let phase_path = work_root.join(format!("{safe_name}-phase.txt"));
        let identity_path = work_root.join(format!("{safe_name}-identity.json"));
        let mut environment = plan.environment.clone();
        environment.insert(
            "CHIBIPOP_OCR_PERF_BACKEND".to_string(),
            plan.child_backend.clone(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_REPORT".to_string(),
            test_path.to_string_lossy().into_owned(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_PHASE_FILE".to_string(),
            phase_path.to_string_lossy().into_owned(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_BACKEND_FILE".to_string(),
            identity_path.to_string_lossy().into_owned(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_FIXTURE_PATH".to_string(),
            options.fixture_path.to_string_lossy().into_owned(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_IDLE_MS".to_string(),
            options.idle.as_millis().to_string(),
        );
        environment.insert(
            "CHIBIPOP_OCR_PERF_PHASE_HOLD_MS".to_string(),
            options.phase_hold.as_millis().to_string(),
        );
        let process = ProcessConfig {
            program: options.child_program.clone(),
            arguments: vec!["--child".to_string(), plan.child_backend.clone()],
            environment,
            duration: options.duration,
            sample_interval: options.sample_interval,
            phase_file: phase_path,
            identity_file: identity_path,
            required_phases: PHASES.iter().map(|phase| (*phase).to_string()).collect(),
            backend: plan.fallback_identity.clone(),
            runner: options.runner.clone(),
            fixture_sha256: FIXTURE_SHA256.to_string(),
        };
        let resources = match monitor_process(&process) {
            Ok(report) => report,
            Err(error) => empty_resource_report(&process, &error.to_string()),
        };
        let command_exit_code = resources.get("child_exit_code").and_then(Value::as_i64);
        let test = read_json(&test_path).and_then(|report| {
            report
                .get("backend_results")
                .and_then(Value::as_array)
                .and_then(|results| {
                    results.iter().find(|result| {
                        result
                            .get("backend")
                            .and_then(|backend| backend.get("id"))
                            .and_then(Value::as_str)
                            == Some(plan.id.as_str())
                    })
                })
                .cloned()
        });
        let result = combine_backend_result(
            plan,
            test.as_ref(),
            &resources,
            command_exit_code,
        );
        for category in result
            .get("failure_categories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|category| *category != "backend-unavailable")
        {
            overall_failures.push(category.to_string());
        }
        backend_results.push(result);
    }
    let baseline = baseline_report(options.baseline_path.as_deref());
    let (baseline_value, baseline_error) = match baseline {
        Ok(value) => (value, None),
        Err(error) => (None, Some(error)),
    };
    let comparisons = comparisons(
        &backend_results,
        &fixture.metadata,
        &options.runner,
        baseline_value.as_ref(),
    );
    if comparisons.iter().any(|comparison| {
        comparison.get("status").and_then(Value::as_str) == Some("not-comparable")
            && comparison.get("output_hashes_stable") == Some(&Value::Bool(true))
            && comparison.get("output_hashes_match") == Some(&Value::Bool(false))
    }) {
        overall_failures.push("baseline-output-mismatch".to_string());
    }
    if comparisons.iter().any(|comparison| {
        comparison.get("status").and_then(Value::as_str)
            == Some("baseline-backend-missing")
    }) {
        overall_failures.push("baseline-input".to_string());
    }
    if baseline_error.is_some() {
        overall_failures.push("baseline-input".to_string());
    }
    let report = json!({
        "schema": "chibipop-ocr-performance/v1",
        "generated_at": system_time_string(SystemTime::now()),
        "mode": "report-only",
        "measurement_target": "rust-native",
        "sample_milliseconds": options.sample_interval.as_millis(),
        "phase_hold_milliseconds": options.phase_hold.as_millis(),
        "runner": options.runner,
        "fixture": fixture.metadata.clone(),
        "comparability": {
            "identity_fields": [
                "runner.image", "runner.os", "runner.architecture", "backend.id",
                "backend.version", "backend.language", "backend.thread_settings",
                "backend.model_hashes", "backend.plugin_hashes", "backend.config_sha256",
                "backend.model_asset_count", "backend.identity_complete", "fixture.sha256",
                "backend.scales",
                "test.stable_hashes.repeated_identical.text_sha256",
                "test.stable_hashes.repeated_identical.geometry_sha256",
                "test.stable_hashes.repeated_identical.text_stable",
                "test.stable_hashes.repeated_identical.geometry_stable"
            ],
            "rule": "Compare reports only when every identity field matches.",
            "build_revision_recorded": true,
            "build_revision_match_required": false,
        },
        "baseline": {
            "provided": options.baseline_path.is_some(),
            "available": baseline_value.is_some(),
            "build_revision_match_required": false,
            "comparisons": comparisons,
            "error": baseline_error.map(|error| redact_reason(&error.to_string())),
        },
        "backends": backend_results,
        "categories": {
            "warning": [
                "working-set-observation", "private-bytes-observation", "latency-observation",
                "cpu-observation", "thread-growth-observation", "handle-growth-observation",
                "unstable-text-hash", "unstable-geometry-hash",
                "identity-incomplete"
            ],
            "failure": [
                "backend-unavailable", "launch-error", "child-failure", "timeout",
                "benchmark-command", "benchmark-report-missing", "benchmark-report-failed",
                "resource-report-missing", "resource-metric-missing", "recognition-error",
                "cleanup-survivor", "cleanup-identity-unverified", "cleanup-identity-mismatch",
                "phase-resource-incomplete", "benchmark-phase-incomplete", "baseline-input",
                "baseline-output-mismatch", "cleanup-signal", "identity-sidecar-write",
                "identity-sidecar-missing", "phase-sidecar-write", "phase-sidecar-read",
                "phase-sidecar-missing", "report-workspace-cleanup"
            ],
            "policy": concat!(
                "Performance categories remain report-only until reviewed numeric ",
                "baselines exist."
            ),
        },
        "thresholds": threshold_report(),
        "failure_categories": unique_strings(&overall_failures),
        "product_goals": {
            "windows_working_set_mib": {
                "target": 100,
                "status": "not-enforced",
                "backend": "windows-ocr"
            },
            "meikiocr_working_set_mib": {
                "target": 200,
                "status": "not-enforced",
                "backend": "meikiocr"
            },
        },
        "privacy": {
            "included": [
                "committed fixture identity",
                "OCR text and canonical geometry hashes",
                "process-tree metrics"
            ],
            "excluded": [
                "personal configs",
                "dictionary data",
                "screenshots",
                "Anki data",
                "absolute personal paths"
            ],
            "path_policy": "Executable and command paths are reduced to non-personal names.",
        },
    });
    Ok(report)
}

fn write_report(options: &ReportOptions, report: &Value) -> Result<()> {
    if let Some(parent) = options.output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("creating OCR report directory {}", redact_path(parent))
        })?;
    }
    std::fs::write(&options.output_path, serde_json::to_vec_pretty(report)?).with_context(|| {
        format!("writing OCR report {}", redact_path(&options.output_path))
    })?;
    Ok(())
}

#[cfg(windows)]
pub fn monitor_process(config: &ProcessConfig) -> Result<Value> {
    process_windows::monitor(config)
}

#[cfg(unix)]
pub fn monitor_process(config: &ProcessConfig) -> Result<Value> {
    process_linux::monitor(config)
}

#[cfg(not(any(windows, unix)))]
pub fn monitor_process(_config: &ProcessConfig) -> Result<Value> {
    bail!("OCR process monitoring is unsupported on this platform")
}

#[derive(Clone)]
pub(crate) struct ResourceRow {
    pub timestamp: String,
    pub phase: String,
    pub backend_id: String,
    pub backend_version: String,
    pub language: Option<String>,
    pub thread_settings: Option<String>,
    pub scales: Option<String>,
    pub fixture_sha256: String,
    pub model_hashes: Option<String>,
    pub plugin_hashes: Option<String>,
    pub runner_image: String,
    pub role: String,
    pub pid: u32,
    pub parent_pid: u32,
    pub process_name: String,
    pub start_ticks: Option<u64>,
    pub executable_path: Option<String>,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub cpu_seconds: Option<f64>,
    pub cpu_percent_one_core: Option<f64>,
    pub cpu_percent: Option<f64>,
    pub threads: Option<u64>,
    pub handles: Option<u64>,
}

pub(crate) struct CleanupOutcome {
    pub stopped_ids: Vec<u32>,
    pub remaining_ids: Vec<u32>,
    pub identity_mismatch_ids: Vec<u32>,
    pub identity_unverified_ids: Vec<u32>,
    pub error: Option<String>,
    pub signal_error: Option<String>,
}

pub(crate) struct ResourceOutcome {
    pub started_at: SystemTime,
    pub root_pid: u32,
    pub child_exit_code: Option<i32>,
    pub child_exited: bool,
    pub timed_out: bool,
    pub rows: Vec<ResourceRow>,
    pub cleanup: CleanupOutcome,
    pub sampling_error: Option<String>,
    pub failure_categories: Vec<String>,
}

pub(crate) fn cpu_totals(rows: &[ResourceRow]) -> (Option<f64>, Option<f64>) {
    if rows.is_empty()
        || rows.iter().any(|row| {
            row.cpu_percent_one_core.is_none() || row.cpu_percent.is_none()
        })
    {
        return (None, None);
    }
    (
        Some(
            rows.iter()
                .filter_map(|row| row.cpu_percent_one_core)
                .sum(),
        ),
        Some(rows.iter().filter_map(|row| row.cpu_percent).sum()),
    )
}

impl ResourceRow {
    pub(crate) fn json(&self) -> Value {
        json!({
            "Timestamp": self.timestamp,
            "Phase": self.phase,
            "BackendId": self.backend_id,
            "BackendVersion": self.backend_version,
            "Language": self.language,
            "ThreadSettings": self.thread_settings,
            "Scales": self.scales,
            "FixtureSha256": self.fixture_sha256,
            "ModelHashes": self.model_hashes,
            "PluginHashes": self.plugin_hashes,
            "RunnerImage": self.runner_image,
            "Role": self.role,
            "Pid": self.pid,
            "ParentPid": self.parent_pid,
            "ProcessName": self.process_name,
            "ProcessStartTimeUtcTicks": self.start_ticks,
            "ExecutablePath": self.executable_path,
            "WorkingSetBytes": self.working_set_bytes,
            "WorkingSetMiB": self.working_set_bytes.map(bytes_to_mib),
            "PrivateBytes": self.private_bytes,
            "PrivateMiB": self.private_bytes.map(bytes_to_mib),
            "CpuSeconds": self.cpu_seconds,
            "CpuPercentOneCore": self.cpu_percent_one_core,
            "CpuPercent": self.cpu_percent,
            "Threads": self.threads,
            "Handles": self.handles,
        })
    }
}

pub(crate) fn resource_report(config: &ProcessConfig, outcome: ResourceOutcome) -> Value {
    let ResourceOutcome {
        started_at,
        root_pid,
        child_exit_code,
        child_exited,
        timed_out,
        rows,
        cleanup,
        sampling_error,
        mut failure_categories,
    } = outcome;
    let phase_data = read_phase_events(&config.phase_file);
    let phase_events = phase_data.events;
    let sidecar = read_json(&config.identity_file);
    if sidecar.is_none() {
        failure_categories.push("identity-sidecar-missing".to_string());
    }
    if phase_data.missing {
        failure_categories.push("phase-sidecar-missing".to_string());
    }
    if phase_data.error.is_some() {
        failure_categories.push("phase-sidecar-read".to_string());
    }
    let backend = sidecar.unwrap_or_else(|| config.backend.clone());
    let records = rows.iter().map(ResourceRow::json).collect::<Vec<_>>();
    if rows.iter().any(|row| {
        row.working_set_bytes.is_none()
            || row.private_bytes.is_none()
            || row.cpu_seconds.is_none()
            || row.threads.is_none()
            || row.handles.is_none()
    }) {
        failure_categories.push("resource-metric-missing".to_string());
    }
    let coverage = phase_coverage(&phase_events, &rows, &config.required_phases);
    let backend_unavailable = backend
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "unavailable");
    if rows.is_empty() && !backend_unavailable {
        failure_categories.push("resource-report-missing".to_string());
    }
    if !backend_unavailable && !coverage["complete"].as_bool().unwrap_or(false) {
        failure_categories.push("phase-resource-incomplete".to_string());
    }
    if cleanup.error.is_some() {
        failure_categories.push("cleanup-identity-unverified".to_string());
        failure_categories.push("resource-report-missing".to_string());
    }
    if cleanup.signal_error.is_some() {
        failure_categories.push("cleanup-signal".to_string());
    }
    let report = json!({
        "schema": "chibipop-ocr-resources/v1",
        "started_at": system_time_string(started_at),
        "sampled_until": system_time_string(SystemTime::now()),
        "root_pid": root_pid,
        "child_exit_code": child_exit_code,
        "child_exited": child_exited,
        "timed_out": timed_out,
        "timeout_seconds": config.duration.as_secs_f64(),
        "timeout_milliseconds": config.duration.as_millis(),
        "failure_categories": unique_strings(&failure_categories),
        "runner": config.runner,
        "backend": backend,
        "fixture_sha256": config.fixture_sha256,
        "command": {
            "file": redact_path(&config.program),
            "arguments": config.arguments.iter().map(|arg| redact_text(arg)).collect::<Vec<_>>(),
            "duration_seconds": config.duration.as_secs_f64(),
            "duration_milliseconds": config.duration.as_millis(),
            "sample_milliseconds": config.sample_interval.as_millis(),
            "logical_processors": std::thread::available_parallelism().map_or(1, |n| n.get()),
        },
        "cleanup_stopped_process_ids": cleanup.stopped_ids,
        "cleanup_remaining_process_ids": cleanup.remaining_ids,
        "cleanup_identity_mismatch_process_ids": cleanup.identity_mismatch_ids,
        "cleanup_identity_unverified_process_ids": cleanup.identity_unverified_ids,
        "cleanup_error": cleanup.error.as_deref().map(redact_reason),
        "cleanup_signal_error": cleanup.signal_error.as_deref().map(redact_reason),
        "sampling_error": sampling_error.as_deref().map(redact_reason),
        "phase_sidecar_error": phase_data.error.as_deref().map(redact_reason),
        "phase_events": phase_events,
        "phase_coverage": coverage,
        "records": records,
        "aggregates": resource_aggregates(&rows),
    });
    report
}

pub(crate) fn resource_aggregates(rows: &[ResourceRow]) -> Vec<Value> {
    let mut output = Vec::new();
    let mut phases = rows.iter().map(|row| row.phase.clone()).collect::<Vec<_>>();
    phases.sort();
    phases.dedup();
    for phase in phases {
        for role in ["parent", "descendant", "total"] {
            let values = rows
                .iter()
                .filter(|row| row.phase == phase && row.role == role)
                .collect::<Vec<_>>();
            if values.is_empty() {
                continue;
            }
            output.push(json!({
                "phase": phase,
                "role": role,
                "sample_count": values.len(),
                "working_set_bytes": metric_stats(
                    values
                        .iter()
                        .filter_map(|row| row.working_set_bytes)
                        .map(|value| value as f64),
                    true
                ),
                "private_bytes": metric_stats(
                    values
                        .iter()
                        .filter_map(|row| row.private_bytes)
                        .map(|value| value as f64),
                    true
                ),
                "cpu_seconds": metric_stats(values.iter().filter_map(|row| row.cpu_seconds), false),
                "cpu_percent": metric_stats(values.iter().filter_map(|row| row.cpu_percent), false),
                "threads": metric_stats(
                    values
                        .iter()
                        .filter_map(|row| row.threads)
                        .map(|value| value as f64),
                    false
                ),
                "handles": metric_stats(
                    values
                        .iter()
                        .filter_map(|row| row.handles)
                        .map(|value| value as f64),
                    false
                ),
            }));
        }
    }
    output
}

fn metric_stats(values: impl Iterator<Item = f64>, bytes: bool) -> Value {
    let mut values = values.collect::<Vec<_>>();
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let median = percentile(&values, 0.5);
    let p95 = percentile(&values, 0.95);
    let peak = values.last().copied().unwrap_or_default();
    let mut result = Map::new();
    result.insert("count".to_string(), json!(values.len()));
    result.insert("median".to_string(), json!(round_metric(median)));
    result.insert("p95".to_string(), json!(round_metric(p95)));
    result.insert("peak".to_string(), json!(round_metric(peak)));
    if bytes {
        result.insert("median_mib".to_string(), json!(round_metric(median / 1_048_576.0)));
        result.insert("p95_mib".to_string(), json!(round_metric(p95 / 1_048_576.0)));
        result.insert("peak_mib".to_string(), json!(round_metric(peak / 1_048_576.0)));
    }
    Value::Object(result)
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn round_metric(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn bytes_to_mib(bytes: u64) -> f64 {
    round_metric(bytes as f64 / 1_048_576.0)
}

fn phase_coverage(events: &[Value], rows: &[ResourceRow], required: &[String]) -> Value {
    let applicable = !required.is_empty();
    let event_names = events
        .iter()
        .filter_map(|event| event.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let mut observed = rows
        .iter()
        .filter(|row| row.role == "total")
        .map(|row| row.phase.clone())
        .collect::<Vec<_>>();
    observed.sort();
    observed.dedup();
    let missing = if applicable {
        required
            .iter()
            .filter(|phase| {
                !observed.contains(phase) || !event_names.contains(&phase.as_str())
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    json!({
        "applicable": applicable,
        "required": required,
        "observed": observed,
        "missing": missing,
        "complete": missing.is_empty(),
    })
}

pub fn read_json(path: &Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(crate) struct PhaseEvents {
    events: Vec<Value>,
    error: Option<String>,
    missing: bool,
}

pub(crate) fn read_phase_events(path: &Path) -> PhaseEvents {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PhaseEvents {
                events: Vec::new(),
                error: None,
                missing: true,
            };
        }
        Err(error) => {
            return PhaseEvents {
                events: Vec::new(),
                error: Some(format!(
                    "reading phase sidecar {} failed: {error}",
                    redact_path(path)
                )),
                missing: false,
            };
        }
    };
    let mut events = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let event = parts.next().unwrap_or_default();
        let timestamp = parts.next().and_then(|value| value.parse::<u64>().ok());
        let name = parts.next().unwrap_or_default();
        if !matches!(event, "start" | "end") || timestamp.is_none() || name.is_empty() {
            return PhaseEvents {
                events: Vec::new(),
                error: Some(format!(
                    "invalid phase sidecar {} line {}",
                    redact_path(path),
                    line_number + 1
                )),
                missing: false,
            };
        }
        events.push(json!({
            "event": event,
            "unix_milliseconds": timestamp.unwrap_or_default(),
            "name": name,
        }));
    }
    PhaseEvents {
        events,
        error: None,
        missing: false,
    }
}

pub(crate) fn phase_name_at(events: Vec<Value>, at: u64) -> String {
    let mut open = BTreeMap::<String, u64>::new();
    let mut intervals = Vec::<(String, u64, u64)>::new();
    for event in events {
        let Some(name) = event.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(timestamp) = event.get("unix_milliseconds").and_then(Value::as_u64) else {
            continue;
        };
        match event.get("event").and_then(Value::as_str) {
            Some("start") => {
                open.insert(name.to_string(), timestamp);
            }
            Some("end") => {
                if let Some(start) = open.remove(name) {
                    intervals.push((name.to_string(), start, timestamp));
                }
            }
            _ => {}
        }
    }
    for (name, start) in open {
        intervals.push((name, start, at));
    }
    intervals
        .into_iter()
        .filter(|(_, start, end)| *start <= at && *end >= at)
        .max_by_key(|(_, start, _)| *start)
        .map_or_else(|| "unknown".to_string(), |(name, _, _)| name)
}

pub fn runner_identity(revision: Option<&str>) -> Value {
    let image = std::env::var("ImageOS").unwrap_or_else(|_| "local".to_string());
    let os = std::env::var("RUNNER_OS").unwrap_or_else(|_| std::env::consts::OS.to_string());
    let architecture = std::env::var("RUNNER_ARCH")
        .unwrap_or_else(|_| std::env::consts::ARCH.to_string());
    let revision = revision
        .filter(|value| {
            value.len() >= 7
                && value.len() <= 64
                && value.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase);
    json!({
        "image": image,
        "os": os,
        "architecture": architecture,
        "build_revision": revision,
    })
}

pub fn build_revision(repo_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (value.len() >= 7 && value.len() <= 64 && value.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(value)
}

pub fn empty_plugin_identity() -> Value {
    json!({
        "model_hashes": null,
        "plugin_hashes": {},
        "config_sha256": null,
        "model_asset_count": 0,
        "identity_complete": false,
        "identity_incomplete_reason": "model-assets-unidentified",
        "thread_settings": {},
        "scales": [1, 2],
    })
}

pub fn plugin_identity(dir: &Path) -> Value {
    let mut plugin_hashes = Map::new();
    for name in ["plugin.toml", "adapter.py"] {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            plugin_hashes.insert(name.to_string(), Value::String(sha256_hex(&bytes)));
        }
    }
    let config_path = dir.join("config.toml");
    let config_bytes = std::fs::read(&config_path).ok();
    let config_hash = config_bytes.as_ref().map(|bytes| sha256_hex(bytes));
    let config = config_bytes
        .as_deref()
        .map(String::from_utf8_lossy)
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let mut thread_settings = Map::new();
    for key in ["threads", "opencv_threads"] {
        if let Some(value) = config.lines().find_map(|line| setting_value(line, key)) {
            thread_settings.insert(key.to_string(), Value::String(value));
        }
    }
    let mut model_files = Vec::new();
    let mut discovery_failed = config_hash.is_none();
    for key in ["model", "model_path", "model_dir", "model_file"] {
        if let Some(value) = path_setting(&config, key) {
            let path = resolve_plugin_path(dir, &value);
            if !collect_model_files(&path, true, &mut model_files) {
                discovery_failed = true;
            }
        }
    }
    let cache = path_setting(&config, "hf_home")
        .map(|value| resolve_plugin_path(dir, &value))
        .unwrap_or_else(|| dir.join("hf-cache"));
    if !collect_model_files(&cache, false, &mut model_files) {
        discovery_failed = true;
    }
    model_files.sort();
    model_files.dedup();
    let mut model_hashes = Map::new();
    for (index, path) in model_files.iter().enumerate() {
        match std::fs::read(path) {
            Ok(bytes) => {
                model_hashes.insert(format!("asset_{index}"), Value::String(sha256_hex(&bytes)));
            }
            Err(_) => discovery_failed = true,
        }
    }
    let identity_complete = config_hash.is_some()
        && !plugin_hashes.is_empty()
        && thread_settings.len() == 2
        && !model_hashes.is_empty()
        && !discovery_failed;
    json!({
        "model_hashes": if model_hashes.is_empty() {
            Value::Null
        } else {
            Value::Object(model_hashes)
        },
        "plugin_hashes": plugin_hashes,
        "config_sha256": config_hash,
        "model_asset_count": model_files.len(),
        "identity_complete": identity_complete,
        "identity_incomplete_reason": if identity_complete {
            Value::Null
        } else {
            Value::String("model-assets-unidentified".to_string())
        },
        "thread_settings": thread_settings,
        "scales": [1, 2],
    })
}

fn path_setting(config: &str, key: &str) -> Option<String> {
    config.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.split('#').next()?.trim();
        let value = value.trim_matches(|character| character == '"' || character == '\'');
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn resolve_plugin_path(dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { dir.join(path) }
}

fn collect_model_files(path: &Path, explicit: bool, files: &mut Vec<PathBuf>) -> bool {
    collect_model_files_at_depth(path, explicit, files, 0)
}

fn collect_model_files_at_depth(
    path: &Path,
    explicit: bool,
    files: &mut Vec<PathBuf>,
    depth: usize,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Ok(kind) = std::fs::metadata(path) else {
        return false;
    };
    if kind.is_file() {
        if explicit || is_model_asset(path) {
            files.push(path.to_path_buf());
        }
        return true;
    }
    if !kind.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    let mut complete = true;
    for entry in entries {
        match entry {
            Ok(entry) => {
                let child = entry.path();
                if entry.file_type().map(|kind| kind.is_dir() || kind.is_file()).unwrap_or(false)
                    && !collect_model_files_at_depth(&child, false, files, depth + 1)
                {
                    complete = false;
                }
            }
            Err(_) => complete = false,
        }
    }
    complete
}

fn is_model_asset(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("onnx" | "safetensors" | "bin" | "pt")
    )
}

fn setting_value(line: &str, key: &str) -> Option<String> {
    let (name, value) = line.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    let value = value.split('#').next()?.trim().trim_matches('"');
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    Some(value.to_string())
}

pub fn new_thresholds() -> Value {
    let metrics = [
        "working_set_bytes",
        "private_bytes",
        "latency_p95_ms",
        "cpu_peak_percent",
        "threads_peak",
        "handles_peak",
        "cleanup_survivor_count",
    ];
    let mut result = Map::new();
    for metric in metrics {
        result.insert(
            metric.to_string(),
            json!({
                "warning": { "enabled": false, "value": null, "operator": "delta_percent_gte" },
                "failure": { "enabled": false, "value": null, "operator": "delta_percent_gte" },
            }),
        );
    }
    Value::Object(result)
}

pub fn threshold_state(delta_percent: Option<f64>, threshold: &Value) -> &'static str {
    let enabled = threshold.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let value = threshold.get("value").and_then(Value::as_f64);
    if !enabled || value.is_none() {
        return "disabled";
    }
    let Some(delta_percent) = delta_percent else {
        return "not-evaluable";
    };
    if threshold.get("operator").and_then(Value::as_str) == Some("delta_percent_gte")
        && delta_percent >= value.unwrap_or_default()
    {
        "exceeded"
    } else {
        "within"
    }
}

pub fn redact_reason(reason: &str) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(start) = find_path_start(reason, cursor) {
        output.push_str(&reason[cursor..start]);
        output.push_str("<path>");
        cursor = path_end(reason, start);
    }
    output.push_str(&reason[cursor..]);
    output
}

pub fn redact_text(value: &str) -> String {
    redact_reason(value)
}

fn find_path_start(value: &str, from: usize) -> Option<usize> {
    value.char_indices().filter(|(index, _)| *index >= from).find_map(|(index, _)| {
        is_path_prefix(value, index).then_some(index)
    })
}

fn is_path_prefix(value: &str, index: usize) -> bool {
    let bytes = value.as_bytes();
    if index >= bytes.len() {
        return false;
    }
    if index + 2 < bytes.len()
        && bytes[index].is_ascii_alphabetic()
        && bytes[index + 1] == b':'
        && (bytes[index + 2] == b'\\' || bytes[index + 2] == b'/')
    {
        return true;
    }
    (index + 1 < bytes.len() && bytes[index] == b'\\' && bytes[index + 1] == b'\\')
        || (bytes[index] == b'/'
            && (index == 0
                || bytes[index - 1].is_ascii_whitespace()
                || matches!(bytes[index - 1], b'=' | b'(' | b'[' | b'"' | b'\'')))
}

fn quoted_path_end(value: &str, start: usize) -> Option<usize> {
    let quote = value[..start].chars().next_back()?;
    if !matches!(quote, '"' | '\'') {
        return None;
    }
    value[start..].find(quote).map(|offset| start + offset)
}

fn path_end(value: &str, start: usize) -> usize {
    if let Some(end) = quoted_path_end(value, start) {
        return end;
    }
    let mut index = start;
    while index < value.len() {
        let character = value[index..].chars().next().unwrap_or_default();
        if matches!(character, '"' | '\'' | ',' | ';' | ')' | ']') {
            break;
        }
        if character.is_whitespace() {
            let next = index + character.len_utf8();
            if is_path_prefix(value, next)
                || (path_component_has_extension(value, start, index)
                    && !path_has_continuation(value, next))
            {
                break;
            }
        }
        index += character.len_utf8();
    }
    index
}

fn path_has_continuation(value: &str, start: usize) -> bool {
    let end = value[start..]
        .find(['"', '\'', ',', ';', ')', ']'])
        .map_or(value.len(), |offset| start + offset);
    value[start..end].contains(['\\', '/'])
}

fn path_component_has_extension(value: &str, start: usize, end: usize) -> bool {
    value[start..end]
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|component| component.find('.').is_some_and(|index| index > 0))
}

pub fn redact_path(path: &Path) -> String {
    let leaf = path.file_name().and_then(|name| name.to_str()).unwrap_or("path");
    format!("<path>/{leaf}")
}

fn unique_strings(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

fn canonical_text(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|line| line.words.iter().map(|word| word.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn canonical_geometry(lines: &[OcrLine]) -> String {
    let mut output = String::new();
    for (line_index, line) in lines.iter().enumerate() {
        for (word_index, word) in line.words.iter().enumerate() {
            let rect = word.rect;
            let _ = write!(
                output,
                "{line_index}:{word_index}:{}:{}:{}:{};",
                rect.x, rect.y, rect.w, rect.h
            );
        }
    }
    output
}

fn validate_lines(lines: &[OcrLine]) -> Result<()> {
    for OcrLine { words } in lines {
        for OcrWord { rect, .. } in words {
            if rect.w <= 0 || rect.h <= 0 {
                bail!("OCR returned a non-positive word rectangle");
            }
        }
    }
    Ok(())
}

fn stable_hashes(samples: &[&Sample]) -> Value {
    let text = samples.first().map(|sample| sample.text_sha256.clone());
    let geometry = samples.first().map(|sample| sample.geometry_sha256.clone());
    let text_stable = !samples.is_empty()
        && samples.iter().all(|sample| Some(&sample.text_sha256) == text.as_ref());
    let geometry_stable = !samples.is_empty()
        && samples.iter().all(|sample| Some(&sample.geometry_sha256) == geometry.as_ref());
    json!({
        "repeated_identical": {
            "count": samples.len(),
            "stable": text_stable && geometry_stable,
            "text_sha256": text,
            "geometry_sha256": geometry,
            "text_stable": text_stable,
            "geometry_stable": geometry_stable,
        }
    })
}

fn latency_aggregates(samples: &[Sample]) -> Value {
    let values = samples.iter().map(|sample| sample.latency_ms).collect::<Vec<_>>();
    let repeated = samples
        .iter()
        .filter(|sample| sample.phase == "repeated-identical-recognition")
        .map(|sample| sample.latency_ms)
        .collect::<Vec<_>>();
    json!({
        "all_recognitions": latency_stats(&values),
        "repeated_identical": latency_stats(&repeated),
    })
}

fn latency_stats(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "count": sorted.len(),
        "median_ms": percentile(&sorted, 0.5),
        "p95_ms": percentile(&sorted, 0.95),
        "peak_ms": sorted.last().copied(),
    })
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn system_time_string(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = duration.as_secs();
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era
        - day_of_era / 1_460
        + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    let millis = duration.subsec_millis();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub(crate) fn timestamp_now() -> String {
    system_time_string(SystemTime::now())
}

fn idle_duration() -> Duration {
    let value = std::env::var("CHIBIPOP_OCR_PERF_IDLE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_MILLISECONDS)
        .min(MAX_IDLE_MILLISECONDS);
    Duration::from_millis(value)
}

fn phase_hold_duration() -> Duration {
    let value = std::env::var("CHIBIPOP_OCR_PERF_PHASE_HOLD_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
        .min(MAX_PHASE_HOLD_MILLISECONDS);
    Duration::from_millis(value)
}

fn read_backend_identity(path: &Path) -> Value {
    read_json(path).unwrap_or_else(|| json!({ "status": "unknown" }))
}

pub(crate) fn backend_identity(path: &Path) -> Value {
    read_backend_identity(path)
}

pub fn sha256_hex(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1,
        0x923f_82a4, 0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
        0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786,
        0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
        0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147,
        0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
        0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
        0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
        0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a,
        0x5b9c_ca4f, 0x682e_6ff3, 0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
        0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
    ];
    let mut message = input.to_vec();
    let bit_length = u64::try_from(message.len()).unwrap_or(u64::MAX).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    for chunk in message.as_chunks::<64>().0 {
        let mut words = [0u32; 64];
        for (index, bytes) in chunk.as_chunks::<4>().0.iter().take(16).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut working: [u32; 8] = state;
        for index in 0..64 {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choose = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority = (working[0] & working[1])
                ^ (working[0] & working[2])
                ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working.copy_within(0..7, 1);
            working[4] = working[4].wrapping_add(temp1);
            working[0] = temp1.wrapping_add(temp2);
        }
        for index in 0..8 {
            state[index] = state[index].wrapping_add(working[index]);
        }
    }
    let mut output = String::with_capacity(64);
    for word in state {
        let _ = write!(output, "{word:08x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standard_abc_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn disabled_thresholds_cover_survivors() {
        let thresholds = new_thresholds();
        assert_eq!(
            thresholds
                .get("cleanup_survivor_count")
                .and_then(|value| value.get("warning"))
                .map(|value| threshold_state(None, value)),
            Some("disabled")
        );
        let enabled = json!({"enabled": true, "value": 10.0, "operator": "delta_percent_gte"});
        assert_eq!(threshold_state(Some(5.0), &enabled), "within");
        assert_eq!(threshold_state(Some(10.0), &enabled), "exceeded");
        assert_eq!(threshold_state(None, &enabled), "not-evaluable");
    }

    #[test]
    fn redaction_hides_drive_and_unc_paths() {
        assert_eq!(
            redact_reason(r#"failed C:\Users\stella\plugin.toml \\server\share\model"#),
            "failed <path> <path>"
        );
        let reason = redact_reason(r#"failed C:\Users\Stella\Private Folder\secret.exe"#);
        assert_eq!(reason, "failed <path>");
        assert!(!reason.contains("Private Folder"));
        assert!(!reason.contains("secret.exe"));
        let cases = [
            (
                r#"error "/Users/Stella/Folder.v2 Name/secret.txt" tail"#,
                r#"error "<path>" tail"#,
            ),
            (
                r#"error "C:\Users\Stella\Folder.v2 Name\secret.exe" tail"#,
                r#"error "<path>" tail"#,
            ),
            (
                r#"error "\\server\share\Folder.v2 Name\secret.bin" tail"#,
                r#"error "<path>" tail"#,
            ),
            (
                r#"error "/Users/Stella/Folder.v2 Name" tail"#,
                r#"error "<path>" tail"#,
            ),
            (
                r#"error "C:\Users\Stella\Folder.v2 Name" tail"#,
                r#"error "<path>" tail"#,
            ),
            (
                r#"error "\\server\share\Folder.v2 Name" tail"#,
                r#"error "<path>" tail"#,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_reason(input), expected);
        }
    }

    struct WrongTextEngine;

    impl chibipop::text::OcrEngine for WrongTextEngine {
        fn recognise(&self, _bgra: &[u8], _w: i32, _h: i32) -> Result<Vec<OcrLine>> {
            Ok(vec![OcrLine {
                words: vec![OcrWord {
                    text: "誤り".to_string(),
                    rect: chibipop::geom::PhysRect {
                        x: 0,
                        y: 0,
                        w: 10,
                        h: 10,
                    },
                }],
            }])
        }

        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "wrong-text"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    #[test]
    fn fixture_text_mismatch_is_a_failure() {
        let mut benchmark = Benchmark::new(
            BenchmarkOptions {
                backend_id: "wrong-text".to_string(),
                version: "test".to_string(),
                language: "ja".to_string(),
                identity: json!({"scales": [1, 2]}),
                fixture: json!({"sha256": FIXTURE_SHA256}),
                phase_file: None,
                identity_file: None,
            },
            vec![0; FIXTURE_WIDTH as usize * FIXTURE_HEIGHT as usize * 4],
        );
        benchmark.idle = Duration::ZERO;
        benchmark.phase_hold = Duration::ZERO;
        let report = benchmark.run_recognition_phases(&WrongTextEngine);
        assert_eq!(report["status"], "failed");
        assert!(strings_from(&report, "failure_categories")
            .contains(&"fixture-text-mismatch".to_string()));
    }

    #[test]
    fn identity_sidecar_write_failure_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-identity-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("identity test directory");
        let mut benchmark = Benchmark::new(
            BenchmarkOptions {
                backend_id: "backend".to_string(),
                version: "test".to_string(),
                language: "ja".to_string(),
                identity: json!({"scales": [1, 2]}),
                fixture: json!({"sha256": FIXTURE_SHA256}),
                phase_file: None,
                identity_file: Some(root.clone()),
            },
            Vec::new(),
        );
        benchmark.set_status("failed");
        let report = benchmark.report("failed", None);
        assert!(strings_from(&report, "failure_categories")
            .contains(&"identity-sidecar-write".to_string()));
        assert!(report
            .get("identity_sidecar_error")
            .is_some_and(|error| !error.is_null()));
        std::fs::remove_dir_all(root).expect("identity test cleanup");
    }

    #[test]
    fn phase_sidecar_write_failure_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-phase-write-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("phase test directory");
        let mut benchmark = Benchmark::new(
            BenchmarkOptions {
                backend_id: "backend".to_string(),
                version: "test".to_string(),
                language: "ja".to_string(),
                identity: json!({"scales": [1, 2]}),
                fixture: json!({"sha256": FIXTURE_SHA256}),
                phase_file: Some(root.clone()),
                identity_file: None,
            },
            Vec::new(),
        );
        benchmark.phase_hold = Duration::ZERO;
        let result = benchmark.phase("one", |_| Ok::<(), anyhow::Error>(()));
        assert!(result.is_err());
        let report = benchmark.report("failed", None);
        assert!(strings_from(&report, "failure_categories")
            .contains(&"phase-sidecar-write".to_string()));
        std::fs::remove_dir_all(root).expect("phase test cleanup");
    }

    #[test]
    fn malformed_baseline_backend_is_rejected() {
        let report = json!({
            "schema": "chibipop-ocr-performance/v1",
            "mode": "report-only",
            "generated_at": "2026-01-01T00:00:00.000Z",
            "runner": {"image": "runner", "os": "windows", "architecture": "x64"},
            "fixture": {
                "id": FIXTURE_ID,
                "sha256": FIXTURE_SHA256,
                "width": FIXTURE_WIDTH,
                "height": FIXTURE_HEIGHT,
                "pixel_format": "bgra8",
                "expected_character": FIXTURE_EXPECTED.to_string(),
            },
            "backends": [{}],
        });
        assert!(validate_baseline_schema(&report).is_err());
    }

    #[test]
    fn stable_output_hash_mismatch_blocks_comparison() {
        let result = |text| {
            json!({
                "backend": {
                    "id": "backend",
                    "version": "1",
                    "language": "ja",
                    "identity_complete": true,
                    "scales": [1, 2],
                },
                "test": {
                    "stable_hashes": {
                        "repeated_identical": {
                            "text_sha256": text,
                            "geometry_sha256": concat!(
                                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                                "bbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                            ),
                            "text_stable": true,
                            "geometry_stable": true,
                        }
                    }
                }
            })
        };
        let fixture = json!({"sha256": "fixture"});
        let runner = json!({"image": "runner", "os": "windows", "architecture": "x64"});
        let current = identity_report(
            &result("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            &fixture,
            &runner,
        );
        let baseline = identity_report(
            &result("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"),
            &fixture,
            &runner,
        );
        assert!(!compare_output_hashes(&current, &baseline));
        assert!(!compare_identity_values(&current, &baseline));
    }

    #[test]
    fn unstable_output_hashes_are_not_comparable() {
        let result = json!({
            "backend": {
                "id": "backend",
                "version": "1",
                "language": "ja",
                "identity_complete": true,
                "scales": [1, 2],
            },
            "test": {
                "stable_hashes": {
                    "repeated_identical": {
                        "text_sha256": null,
                        "geometry_sha256": null,
                        "text_stable": false,
                        "geometry_stable": false,
                    }
                }
            }
        });
        let fixture = json!({"sha256": "fixture"});
        let runner = json!({"image": "runner", "os": "windows", "architecture": "x64"});
        let identity = identity_report(&result, &fixture, &runner);
        assert!(!compare_identity_values(&identity, &identity));
        assert!(!compare_output_hashes(&identity, &identity));
        let baseline = json!({
            "runner": runner,
            "fixture": fixture,
            "backends": [result.clone()]
        });
        let comparison = comparisons(
            &[result],
            &json!({"sha256": "fixture"}),
            &json!({"image": "runner", "os": "windows", "architecture": "x64"}),
            Some(&baseline),
        );
        assert_eq!(
            comparison[0].get("status").and_then(Value::as_str),
            Some("not-comparable")
        );
    }

    #[test]
    fn low_sample_intervals_get_a_coverage_margin() {
        for _ in 0..8 {
            assert_eq!(
                minimum_phase_hold(Duration::from_millis(50)),
                Duration::from_secs(1)
            );
        }
        assert_eq!(
            minimum_phase_hold(Duration::from_millis(300)),
            Duration::from_millis(1200)
        );
    }

    #[test]
    fn cpu_totals_are_null_when_a_rate_is_missing() {
        let row = |rate| ResourceRow {
            timestamp: "now".to_string(),
            phase: "phase".to_string(),
            backend_id: "backend".to_string(),
            backend_version: "version".to_string(),
            language: Some("ja".to_string()),
            thread_settings: None,
            scales: Some("1,2".to_string()),
            fixture_sha256: "fixture".to_string(),
            model_hashes: None,
            plugin_hashes: None,
            runner_image: "local".to_string(),
            role: "parent".to_string(),
            pid: 1,
            parent_pid: 0,
            process_name: "process".to_string(),
            start_ticks: Some(1),
            executable_path: None,
            working_set_bytes: Some(1),
            private_bytes: Some(1),
            cpu_seconds: Some(1.0),
            cpu_percent_one_core: rate,
            cpu_percent: rate.map(|value| value / 2.0),
            threads: Some(1),
            handles: Some(1),
        };
        assert_eq!(cpu_totals(&[row(None), row(Some(2.0))]), (None, None));
        assert_eq!(cpu_totals(&[row(Some(1.0)), row(Some(2.0))]), (Some(3.0), Some(1.5)));
    }

    #[test]
    fn timeout_fields_keep_subsecond_precision() {
        let config = ProcessConfig {
            program: PathBuf::from("tool"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            duration: Duration::from_millis(250),
            sample_interval: Duration::from_millis(50),
            phase_file: PathBuf::from("phase"),
            identity_file: PathBuf::from("identity"),
            required_phases: Vec::new(),
            backend: json!({
                "id": "backend",
                "version": "1",
                "language": "ja",
                "status": "unavailable",
            }),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        };
        let report = resource_report(
            &config,
            ResourceOutcome {
                started_at: SystemTime::UNIX_EPOCH,
                root_pid: 0,
                child_exit_code: None,
                child_exited: false,
                timed_out: false,
                rows: Vec::new(),
                cleanup: CleanupOutcome {
                    stopped_ids: Vec::new(),
                    remaining_ids: Vec::new(),
                    identity_mismatch_ids: Vec::new(),
                    identity_unverified_ids: Vec::new(),
                    error: None,
                    signal_error: None,
                },
                sampling_error: None,
                failure_categories: Vec::new(),
            },
        );
        assert_eq!(report["timeout_seconds"], 0.25);
        assert_eq!(report["timeout_milliseconds"], 250);
        assert!(report["failure_categories"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| {
                value == "identity-sidecar-missing"
            })));
    }

    fn phase_test_config(path: PathBuf) -> ProcessConfig {
        ProcessConfig {
            program: PathBuf::from("tool"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            duration: Duration::from_secs(1),
            sample_interval: Duration::from_millis(50),
            phase_file: path,
            identity_file: PathBuf::from("missing-identity"),
            required_phases: vec!["one".to_string()],
            backend: json!({
                "id": "backend",
                "version": "1",
                "language": "ja",
                "status": "available",
            }),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        }
    }

    fn phase_test_outcome() -> ResourceOutcome {
        ResourceOutcome {
            started_at: SystemTime::UNIX_EPOCH,
            root_pid: 0,
            child_exit_code: Some(0),
            child_exited: true,
            timed_out: false,
            rows: Vec::new(),
            cleanup: CleanupOutcome {
                stopped_ids: Vec::new(),
                remaining_ids: Vec::new(),
                identity_mismatch_ids: Vec::new(),
                identity_unverified_ids: Vec::new(),
                error: None,
                signal_error: None,
            },
            sampling_error: None,
            failure_categories: Vec::new(),
        }
    }

    #[test]
    fn empty_phase_sidecar_is_a_hard_incomplete_failure() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-empty-phase-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("empty phase directory");
        let phase = root.join("phase.txt");
        std::fs::write(&phase, b"").expect("empty phase sidecar");
        let report = resource_report(&phase_test_config(phase), phase_test_outcome());
        assert_eq!(report["phase_coverage"]["complete"], Value::Bool(false));
        assert!(report["failure_categories"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| {
                value == "phase-resource-incomplete"
            })));
        std::fs::remove_dir_all(root).expect("empty phase cleanup");
    }

    #[test]
    fn unreadable_phase_sidecar_is_a_hard_failure() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-unreadable-phase-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("unreadable phase directory");
        let phase = root.join("phase.txt");
        std::fs::create_dir(&phase).expect("unreadable phase sidecar");
        let report = resource_report(&phase_test_config(phase), phase_test_outcome());
        assert_eq!(report["phase_coverage"]["complete"], Value::Bool(false));
        assert!(report["failure_categories"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == "phase-sidecar-read")));
        std::fs::remove_dir_all(root).expect("unreadable phase cleanup");
    }


    #[test]
    fn percentile_uses_upper_rank() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.95), 4.0);
    }

    #[test]
    fn phase_events_assign_the_latest_open_phase() {
        let events = vec![
            json!({"event": "start", "unix_milliseconds": 10, "name": "one"}),
            json!({"event": "end", "unix_milliseconds": 20, "name": "one"}),
            json!({"event": "start", "unix_milliseconds": 21, "name": "two"}),
        ];
        assert_eq!(phase_name_at(events, 25), "two");
    }

    #[test]
    fn incomplete_phase_coverage_names_the_missing_phase() {
        let row = ResourceRow {
            timestamp: "now".to_string(),
            phase: "one".to_string(),
            backend_id: "backend".to_string(),
            backend_version: "version".to_string(),
            language: None,
            thread_settings: None,
            scales: None,
            fixture_sha256: "fixture".to_string(),
            model_hashes: None,
            plugin_hashes: None,
            runner_image: "local".to_string(),
            role: "total".to_string(),
            pid: 0,
            parent_pid: 0,
            process_name: "total".to_string(),
            start_ticks: None,
            executable_path: None,
            working_set_bytes: Some(1),
            private_bytes: Some(1),
            cpu_seconds: Some(1.0),
            cpu_percent_one_core: Some(1.0),
            cpu_percent: Some(1.0),
            threads: Some(1),
            handles: Some(1),
        };
        let events = vec![json!({
            "event": "start",
            "unix_milliseconds": 1,
            "name": "one",
        })];
        let coverage = phase_coverage(&events, &[row], &["one".to_string(), "two".to_string()]);
        assert_eq!(coverage["complete"], Value::Bool(false));
        assert_eq!(coverage["missing"], json!(["two"]));
    }

    #[test]
    fn aggregates_emit_median_p95_and_peak_for_totals() {
        let row = |value| ResourceRow {
            timestamp: "now".to_string(),
            phase: "phase".to_string(),
            backend_id: "backend".to_string(),
            backend_version: "version".to_string(),
            language: Some("ja".to_string()),
            thread_settings: None,
            scales: None,
            fixture_sha256: "fixture".to_string(),
            model_hashes: None,
            plugin_hashes: None,
            runner_image: "local".to_string(),
            role: "total".to_string(),
            pid: 0,
            parent_pid: 0,
            process_name: "process-tree-total".to_string(),
            start_ticks: None,
            executable_path: None,
            working_set_bytes: Some(value),
            private_bytes: Some(value),
            cpu_seconds: Some(value as f64),
            cpu_percent_one_core: Some(value as f64),
            cpu_percent: Some(value as f64),
            threads: Some(value),
            handles: Some(value),
        };
        let aggregates = resource_aggregates(&[row(1), row(2), row(3)]);
        let aggregate = &aggregates[0];
        for key in ["working_set_bytes", "private_bytes", "cpu_seconds", "threads", "handles"] {
            let stats = aggregate.get(key).expect("metric");
            assert!(stats.get("median").is_some());
            assert!(stats.get("p95").is_some());
            assert!(stats.get("peak").is_some());
        }
    }

    #[test]
    fn missing_benchmark_reports_are_hard_failures() {
        let plan = BackendPlan {
            id: "backend".to_string(),
            child_backend: "backend".to_string(),
            fallback_identity: json!({"id": "backend", "version": "1", "language": "ja"}),
            environment: BTreeMap::new(),
        };
        let config = ProcessConfig {
            program: PathBuf::from("tool"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            duration: Duration::from_secs(1),
            sample_interval: Duration::from_millis(50),
            phase_file: PathBuf::from("phase"),
            identity_file: PathBuf::from("identity"),
            required_phases: Vec::new(),
            backend: plan.fallback_identity.clone(),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        };
        let report = combine_backend_result(
            &plan,
            None,
            &empty_resource_report(&config, "missing"),
            None,
        );
        assert!(strings_from(&report, "failure_categories")
            .contains(&"benchmark-report-missing".to_string()));
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn native_sampler_reports_a_process_tree_total() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-sampler-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temporary sampler directory");
        #[cfg(windows)]
        let (program, arguments) = (
            PathBuf::from("cmd.exe"),
            vec![
                "/c".to_string(),
                "ping".to_string(),
                "127.0.0.1".to_string(),
                "-n".to_string(),
                "2".to_string(),
            ],
        );
        #[cfg(unix)]
        let (program, arguments) = (PathBuf::from("sleep"), vec!["1".to_string()]);
        let config = ProcessConfig {
            program,
            arguments,
            environment: BTreeMap::new(),
            duration: Duration::from_secs(2),
            sample_interval: Duration::from_millis(50),
            phase_file: root.join("phase.txt"),
            identity_file: root.join("identity.json"),
            required_phases: Vec::new(),
            backend: json!({"id": "test", "version": "test", "language": "ja"}),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        };
        let report = monitor_process(&config).expect("native sampler");
        assert_eq!(report.get("schema").and_then(Value::as_str), Some("chibipop-ocr-resources/v1"));
        assert!(report
            .get("records")
            .and_then(Value::as_array)
            .is_some_and(|records| {
                records
                    .iter()
                    .any(|record| record.get("Role") == Some(&Value::String("total".to_string())))
            }));
        assert!(report
            .get("cleanup_remaining_process_ids")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty));
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn native_sampler_records_child_failure() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-failure-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temporary sampler directory");
        #[cfg(windows)]
        let (program, arguments) = (
            PathBuf::from("cmd.exe"),
            vec!["/c".to_string(), "exit".to_string(), "7".to_string()],
        );
        #[cfg(unix)]
        let (program, arguments) = (
            PathBuf::from("sh"),
            vec!["-c".to_string(), "exit 7".to_string()],
        );
        let config = ProcessConfig {
            program,
            arguments,
            environment: BTreeMap::new(),
            duration: Duration::from_secs(2),
            sample_interval: Duration::from_millis(50),
            phase_file: root.join("phase.txt"),
            identity_file: root.join("identity.json"),
            required_phases: Vec::new(),
            backend: json!({"id": "test", "version": "test"}),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        };
        let report = monitor_process(&config).expect("native sampler");
        assert_eq!(report.get("child_exit_code").and_then(Value::as_i64), Some(7));
        assert!(report
            .get("failure_categories")
            .and_then(Value::as_array)
            .is_some_and(|categories| {
                categories.iter().any(|category| category == "child-failure")
            }));
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn native_sampler_times_out_and_cleans_the_child() {
        let root = std::env::temp_dir().join(format!(
            "chibipop-ocr-timeout-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temporary sampler directory");
        #[cfg(windows)]
        let (program, arguments) = (
            PathBuf::from("cmd.exe"),
            vec![
                "/c".to_string(),
                "ping".to_string(),
                "127.0.0.1".to_string(),
                "-n".to_string(),
                "10".to_string(),
            ],
        );
        #[cfg(unix)]
        let (program, arguments) = (PathBuf::from("sleep"), vec!["3".to_string()]);
        let config = ProcessConfig {
            program,
            arguments,
            environment: BTreeMap::new(),
            duration: Duration::from_millis(200),
            sample_interval: Duration::from_millis(50),
            phase_file: root.join("phase.txt"),
            identity_file: root.join("identity.json"),
            required_phases: Vec::new(),
            backend: json!({"id": "test", "version": "test"}),
            runner: json!({"image": "local"}),
            fixture_sha256: "fixture".to_string(),
        };
        let report = monitor_process(&config).expect("native sampler");
        assert_eq!(report.get("timed_out").and_then(Value::as_bool), Some(true));
        assert!(report
            .get("failure_categories")
            .and_then(Value::as_array)
            .is_some_and(|categories| categories.iter().any(|category| category == "timeout")));
        assert!(report
            .get("cleanup_remaining_process_ids")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn matching_baselines_ignore_build_revision() {
        let backend = json!({
            "id": "windows-ocr",
            "version": "system",
            "language": "ja",
            "identity_complete": true,
            "scales": [1, 2],
        });
        let current = json!({
            "backend": backend,
            "status": "available",
            "test": {
                "latency": {"repeated_identical": {"p95_ms": 1.0}},
                "stable_hashes": {
                    "repeated_identical": {
                        "text_sha256": concat!(
                            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        ),
                        "geometry_sha256": concat!(
                            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "bbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        ),
                        "text_stable": true,
                        "geometry_stable": true
                    }
                }
            },
            "resources": {"aggregates": []},
        });
        let baseline = json!({
            "runner": {
                "image": "runner",
                "os": "windows",
                "architecture": "x64",
                "build_revision": "old"
            },
            "fixture": {"sha256": "fixture"},
            "backends": [current.clone()],
        });
        let runner = json!({
            "image": "runner",
            "os": "windows",
            "architecture": "x64",
            "build_revision": "new"
        });
        let comparisons = comparisons(
            &[current],
            &json!({"sha256": "fixture"}),
            &runner,
            Some(&baseline),
        );
        assert_eq!(comparisons[0].get("status").and_then(Value::as_str), Some("comparable"));
        assert_eq!(comparisons[0].get("build_revision_match_required"), Some(&Value::Bool(false)));
    }
}
