//! Measures OCR latency on one committed, reproducible pixel buffer.
//!
//! The ignored test stays independent from the application database and UI.
//! The report records observations only. It does not enforce performance bars.
#![cfg(windows)]

use anyhow::{bail, Context, Result};
use chibipop::text::layout::{OcrLine, OcrWord};
use chibipop::text::OcrEngine;
use chibipop_windows::plugin::{host, manifest, text::PluginText};
use chibipop_windows::text::ocr::WinrtOcr;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FIXTURE_WIDTH: i32 = 400;
const FIXTURE_HEIGHT: i32 = 120;
const FIXTURE_ID: &str = "japanese_bgra.bin";
const FIXTURE_SHA256: &str = "e204bfc27c9c333f792a04040fca1100ebb788b7b199a57cb0a92afaa64250e1";
const FIXTURE_EXPECTED: char = '昨';
const REPEATED_COUNT: usize = 7;
const DEFAULT_IDLE_MILLISECONDS: u64 = 300;
const MAX_IDLE_MILLISECONDS: u64 = 5_000;
const MAX_PHASE_HOLD_MILLISECONDS: u64 = 60_000;

#[test]
#[ignore = "Runs installed OCR engines for performance measurements"]
fn fixed_pixels_engine_latency() -> Result<()> {
    let fixture = load_fixture()?;
    let requested = std::env::var("CHIBIPOP_OCR_PERF_BACKEND")
        .unwrap_or_else(|_| "all".to_string());
    let reports = match requested.as_str() {
        "windows" => vec![run_windows(&fixture)],
        "meikiocr" => vec![run_meikiocr(&fixture)],
        "all" => vec![run_windows(&fixture), run_meikiocr(&fixture)],
        other => bail!("unsupported OCR performance backend: {other}"),
    };
    let report = json!({
        "schema": "chibipop-ocr-performance/v1",
        "mode": "report-only",
        "fixture": fixture.metadata,
        "backend_results": reports,
    });
    if let Some(path) = std::env::var_os("CHIBIPOP_OCR_PERF_REPORT") {
        let path = PathBuf::from(path);
        std::fs::write(&path, serde_json::to_vec_pretty(&report)?).with_context(|| {
            format!("writing OCR performance report {}", path.display())
        })?;
    }
    println!("{report}");
    Ok(())
}

struct Fixture {
    pixels: Vec<u8>,
    metadata: Value,
}

fn load_fixture() -> Result<Fixture> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE_ID);
    let pixels = std::fs::read(&path).with_context(|| format!("reading {FIXTURE_ID}"))?;
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

fn run_windows(fixture: &Fixture) -> Value {
    let mut benchmark = Benchmark::new(
        "windows-ocr",
        "system",
        "ja",
        fixture.metadata.clone(),
        phase_file(),
        json!({
            "model_hashes": {},
            "plugin_hashes": {},
            "config_sha256": null,
            "model_asset_count": 0,
            "identity_complete": true,
            "identity_incomplete_reason": null,
            "thread_settings": {"runtime": "system"},
            "scales": [1, 2],
        }),
    );
    let result = benchmark.phase("cold-startup", |_| {
        WinrtOcr::new("ja").map(|engine| Box::new(engine) as Box<dyn OcrEngine>)
    });
    match result {
        Ok(engine) => benchmark.run_recognition_phases(&*engine, fixture),
        Err(_) => benchmark.unavailable("backend-unavailable"),
    }
}

fn run_meikiocr(fixture: &Fixture) -> Value {
    let directory = std::env::var_os("CHIBIPOP_BENCH_PLUGIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let identity = directory
        .as_deref()
        .map(plugin_identity)
        .unwrap_or_else(empty_plugin_identity);
    let mut benchmark = Benchmark::new(
        "meikiocr",
        "manifest",
        "ja",
        fixture.metadata.clone(),
        phase_file(),
        identity,
    );
    let Some(dir) = directory else {
        return benchmark.unavailable("backend-unavailable");
    };
    let result = benchmark.phase("cold-startup", |_| {
        let manifest_path = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&manifest_path)
            .context("reading the benchmark plugin manifest")?;
        let spec = manifest::parse(&text).context("parsing the benchmark plugin manifest")?;
        let process = host::spawn(&spec, &dir).context("starting the benchmark plugin")?;
        let version = if process.ready().version.is_empty() {
            spec.version.clone()
        } else {
            process.ready().version.clone()
        };
        Ok((Box::new(PluginText::new(process, &spec)) as Box<dyn OcrEngine>, version))
    });
    match result {
        Ok((engine, version)) => {
            benchmark.set_version(version);
            benchmark.run_recognition_phases(&*engine, fixture)
        }
        Err(_) => benchmark.unavailable("backend-unavailable"),
    }
}

fn phase_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_PHASE_FILE").map(PathBuf::from)
}

fn identity_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_BACKEND_FILE").map(PathBuf::from)
}

fn plugin_identity(dir: &Path) -> Value {
    let mut plugin_hashes = serde_json::Map::new();
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
    let mut thread_settings = serde_json::Map::new();
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
    let mut model_hashes = serde_json::Map::new();
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
    let model_hashes = if model_hashes.is_empty() {
        Value::Null
    } else {
        Value::Object(model_hashes)
    };
    json!({
        "model_hashes": model_hashes,
        "plugin_hashes": plugin_hashes,
        "config_sha256": config_hash,
        "model_asset_count": model_files.len(),
        "identity_complete": identity_complete,
        "identity_incomplete_reason": if identity_complete { Value::Null } else { Value::String("model-assets-unidentified".to_string()) },
        "thread_settings": thread_settings,
        "scales": [1, 2],
    })
}

fn empty_plugin_identity() -> Value {
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

fn collect_model_files_at_depth(path: &Path, explicit: bool, files: &mut Vec<PathBuf>, depth: usize) -> bool {
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
        path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase).as_deref(),
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

struct Benchmark {
    backend_id: &'static str,
    version: String,
    language: &'static str,
    fixture: Value,
    identity: Value,
    status: String,
    origin: Instant,
    idle: Duration,
    phase_hold: Duration,
    phase_file: Option<PathBuf>,
    identity_file: Option<PathBuf>,
    phases: Vec<Value>,
    samples: Vec<Sample>,
    warnings: Vec<&'static str>,
    failure_categories: Vec<&'static str>,
}

impl Benchmark {
    fn new(
        backend_id: &'static str,
        version: &str,
        language: &'static str,
        fixture: Value,
        phase_file: Option<PathBuf>,
        identity: Value,
    ) -> Self {
        let benchmark = Self {
            backend_id,
            version: version.to_string(),
            language,
            fixture,
            identity,
            status: "starting".to_string(),
            origin: Instant::now(),
            idle: idle_duration(),
            phase_hold: phase_hold_duration(),
            phase_file,
            identity_file: identity_file(),
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
        result
    }

    fn write_phase_event(&self, event: &str, name: &str, at_unix_ms: u64) {
        if let Some(path) = &self.phase_file {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{event}\t{at_unix_ms}\t{name}");
            }
        }
    }

    fn write_identity(&self) {
        if let Some(path) = &self.identity_file {
            let _ = std::fs::write(path, serde_json::to_vec(&self.backend_value()).unwrap_or_default());
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

    fn run_recognition_phases(&mut self, engine: &dyn OcrEngine, fixture: &Fixture) -> Value {
        let result = (|| {
            let idle = self.idle;
            self.phase("post-handshake-idle", |_| {
                std::thread::sleep(idle);
                Ok(())
            })?;
            self.phase("first-recognition", |benchmark| {
                benchmark.record(
                    engine,
                    "first-recognition",
                    &fixture.pixels,
                    FIXTURE_WIDTH,
                    FIXTURE_HEIGHT,
                    1,
                )
            })?;
            self.phase("repeated-identical-recognition", |benchmark| {
                for _ in 0..REPEATED_COUNT {
                    benchmark.record(
                        engine,
                        "repeated-identical-recognition",
                        &fixture.pixels,
                        FIXTURE_WIDTH,
                        FIXTURE_HEIGHT,
                        1,
                    )?;
                }
                Ok(())
            })?;
            self.phase("uncached-recognition", |benchmark| {
                let (pixels, width, height) = chibipop::text::source::upscale_by(
                    &fixture.pixels,
                    FIXTURE_WIDTH,
                    FIXTURE_HEIGHT,
                    2,
                );
                benchmark.record(engine, "uncached-recognition", &pixels, width, height, 2)
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
        self.report("available", None)
    }

    fn record(
        &mut self,
        engine: &dyn OcrEngine,
        phase: &'static str,
        pixels: &[u8],
        width: i32,
        height: i32,
        scale: i32,
    ) -> Result<()> {
        let started = Instant::now();
        let lines = engine.recognise(pixels, width, height)?;
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

    fn unavailable(&mut self, category: &'static str) -> Value {
        self.set_status("unavailable");
        self.failure_categories.push(category);
        self.report("unavailable", None)
    }

    fn report(&self, status: &str, reason: Option<String>) -> Value {
        let repeated = self
            .samples
            .iter()
            .filter(|sample| sample.phase == "repeated-identical-recognition")
            .collect::<Vec<_>>();
        let stable = stable_hashes(&repeated);
        let mut report = json!({
            "status": status,
            "backend": self.backend_value(),
            "fixture": self.fixture,
            "phases": self.phases,
            "recognitions": self.samples.iter().map(Sample::json).collect::<Vec<_>>(),
            "latency": latency_aggregates(&self.samples),
            "stable_hashes": stable,
            "warning_categories": self.warnings,
            "failure_categories": self.failure_categories,
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

fn stable_hashes(samples: &[&Sample]) -> Value {
    let text = samples.first().map(|sample| sample.text_sha256.clone());
    let geometry = samples.first().map(|sample| sample.geometry_sha256.clone());
    let text_stable = !samples.is_empty()
        && samples.iter().all(|sample| Some(&sample.text_sha256) == text.as_ref());
    let geometry_stable = !samples.is_empty()
        && samples
            .iter()
            .all(|sample| Some(&sample.geometry_sha256) == geometry.as_ref());
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
        "all_recognitions": aggregate_values(&values),
        "repeated_identical": aggregate_values(&repeated),
    })
}

fn aggregate_values(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = percentile(&sorted, 0.50);
    let p95 = percentile(&sorted, 0.95);
    let peak = sorted.last().copied().unwrap_or_default();
    json!({
        "count": sorted.len(),
        "median_ms": median,
        "p95_ms": p95,
        "peak_ms": peak,
    })
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
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

fn redact_reason(reason: &str) -> String {
    reason
        .split_whitespace()
        .map(|part| {
            if part.starts_with("\\\\")
                || part.contains(":\\")
                || part.contains(":/")
                || part.starts_with('/')
            {
                "<path>"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn sha256_hex(input: &[u8]) -> String {
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
    use super::{redact_reason, sha256_hex};

    #[test]
    fn sha256_matches_the_standard_abc_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn redact_reason_hides_drive_and_unc_paths() {
        assert_eq!(
            redact_reason(r#"failed C:\Users\stella\plugin.toml \\server\share\model"#),
            "failed <path> <path>"
        );
    }
}
