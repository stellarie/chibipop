// Native Windows wrapper.
#[cfg(windows)]
#[path = "../../ocr-performance/mod.rs"]
mod monitor;

#[cfg(windows)]
use anyhow::{bail, Context, Result};
#[cfg(windows)]
use chibipop::text::OcrEngine;
#[cfg(windows)]
use chibipop_windows::plugin::{host, manifest, text::PluginText};
#[cfg(windows)]
use chibipop_windows::text::ocr::WinrtOcr;
#[cfg(windows)]
use serde_json::{json, Value};
#[cfg(windows)]
use std::collections::BTreeMap;
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
fn main() {
    let result = if std::env::args().nth(1).as_deref() == Some("--child") {
        child_main()
    } else {
        report_main()
    };
    if let Err(error) = result {
        eprintln!("{}", monitor::redact_reason(&error.to_string()));
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn child_main() -> Result<()> {
    let backend = std::env::args().nth(2).context("missing OCR backend")?;
    let fixture = monitor::load_fixture(&fixture_path())?;
    let result = match backend.as_str() {
        "windows" | "windows-ocr" => run_windows(&fixture),
        "meikiocr" => run_meikiocr(&fixture),
        other => bail!("unsupported OCR performance backend: {other}"),
    };
    let report = json!({
        "schema": "chibipop-ocr-performance/v1",
        "mode": "report-only",
        "fixture": fixture.metadata,
        "backend_results": [result],
    });
    monitor::write_child_report(&report)
}

#[cfg(windows)]
fn report_main() -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_path = fixture_path();
    let child_program = std::env::current_exe().context("locating the OCR report executable")?;
    let plugin = std::env::var_os("CHIBIPOP_OCR_PERF_PLUGIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let mut plugin_environment = BTreeMap::new();
    if let Some(path) = &plugin {
        plugin_environment.insert(
            "CHIBIPOP_BENCH_PLUGIN".to_string(),
            path.to_string_lossy().into_owned(),
        );
    }
    let windows_identity = json!({
        "id": "windows-ocr",
        "version": "system",
        "language": "ja",
        "geometry": true,
        "model_hashes": {},
        "plugin_hashes": {},
        "config_sha256": null,
        "model_asset_count": 0,
        "identity_complete": true,
        "identity_incomplete_reason": null,
        "thread_settings": {"runtime": "system"},
        "scales": [1, 2],
    });
    let plugin_identity = plugin
        .as_deref()
        .map(monitor::plugin_identity)
        .unwrap_or_else(monitor::empty_plugin_identity);
    let meiki_identity = with_backend_identity(
        "meikiocr",
        "manifest",
        "ja",
        plugin_identity,
    );
    let duration = env_seconds("CHIBIPOP_OCR_PERF_DURATION_SECONDS", 60, 1, 3_600);
    let sample = env_duration("CHIBIPOP_OCR_PERF_SAMPLE_MS", 100, 10, 15_000);
    let idle = env_duration("CHIBIPOP_OCR_PERF_IDLE_MS", 300, 0, 5_000);
    let minimum_phase_hold = monitor::minimum_phase_hold(sample);
    let minimum_phase_hold_millis =
        u64::try_from(minimum_phase_hold.as_millis()).unwrap_or(u64::MAX);
    let phase_hold = env_duration(
        "CHIBIPOP_OCR_PERF_PHASE_HOLD_MS",
        minimum_phase_hold_millis,
        minimum_phase_hold_millis,
        60_000,
    );
    let output_path = std::env::var_os("CHIBIPOP_OCR_PERF_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ocr-performance.json"));
    let baseline_path = std::env::var_os("CHIBIPOP_OCR_PERF_BASELINE").map(PathBuf::from);
    let runner = monitor::runner_identity(monitor::build_revision(&repo_root).as_deref());
    let report = monitor::run_report(&monitor::ReportOptions {
        fixture_path,
        child_program,
        backends: vec![
            monitor::BackendPlan {
                id: "windows-ocr".to_string(),
                child_backend: "windows".to_string(),
                fallback_identity: windows_identity,
                environment: BTreeMap::new(),
            },
            monitor::BackendPlan {
                id: "meikiocr".to_string(),
                child_backend: "meikiocr".to_string(),
                fallback_identity: meiki_identity,
                environment: plugin_environment,
            },
        ],
        output_path: output_path.clone(),
        baseline_path,
        duration,
        sample_interval: sample,
        idle,
        phase_hold,
        runner,
    })?;
    println!("Wrote {}", monitor::redact_path(&output_path));
    let failures = report
        .get("failure_categories")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if !failures.is_empty() {
        println!("Failure categories: {}", failures.join(", "));
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(windows)]
fn with_backend_identity(id: &str, version: &str, language: &str, identity: Value) -> Value {
    let mut backend = json!({
        "id": id,
        "version": version,
        "language": language,
        "geometry": true,
    });
    if let (Some(destination), Some(source)) = (backend.as_object_mut(), identity.as_object()) {
        for (key, value) in source {
            destination.insert(key.clone(), value.clone());
        }
    }
    backend
}

#[cfg(windows)]
fn fixture_path() -> PathBuf {
    std::env::var_os("CHIBIPOP_OCR_PERF_FIXTURE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join(monitor::FIXTURE_ID)
        })
}

#[cfg(windows)]
fn run_windows(fixture: &monitor::Fixture) -> Value {
    monitor::run_benchmark_with_fixture(
        monitor::BenchmarkOptions {
            backend_id: "windows-ocr".to_string(),
            version: "system".to_string(),
            language: "ja".to_string(),
            identity: json!({
                "model_hashes": {},
                "plugin_hashes": {},
                "config_sha256": null,
                "model_asset_count": 0,
                "identity_complete": true,
                "identity_incomplete_reason": null,
                "thread_settings": {"runtime": "system"},
                "scales": [1, 2],
            }),
            fixture: Value::Null,
            phase_file: phase_file(),
            identity_file: identity_file(),
        },
        fixture,
        || WinrtOcr::new("ja").map(|engine| {
            (Box::new(engine) as Box<dyn OcrEngine>, "system".to_string())
        }),
    )
}

#[cfg(windows)]
fn run_meikiocr(fixture: &monitor::Fixture) -> Value {
    let directory = std::env::var_os("CHIBIPOP_BENCH_PLUGIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let identity = directory
        .as_deref()
        .map(monitor::plugin_identity)
        .unwrap_or_else(monitor::empty_plugin_identity);
    let Some(dir) = directory else {
        return monitor::run_benchmark_with_fixture(
            monitor::BenchmarkOptions {
                backend_id: "meikiocr".to_string(),
                version: "manifest".to_string(),
                language: "ja".to_string(),
                identity,
                fixture: Value::Null,
                phase_file: phase_file(),
                identity_file: identity_file(),
            },
            fixture,
            || Err(anyhow::anyhow!("MeikiOCR plugin is not configured")),
        );
    };
    monitor::run_benchmark_with_fixture(
        monitor::BenchmarkOptions {
            backend_id: "meikiocr".to_string(),
            version: "manifest".to_string(),
            language: "ja".to_string(),
            identity,
            fixture: Value::Null,
            phase_file: phase_file(),
            identity_file: identity_file(),
        },
        fixture,
        || {
            let text = std::fs::read_to_string(dir.join("plugin.toml"))
                .context("reading the benchmark plugin manifest")?;
            let spec = manifest::parse(&text).context("parsing the benchmark plugin manifest")?;
            let process = host::spawn(&spec, &dir).context("starting the benchmark plugin")?;
            let version = if process.ready().version.is_empty() {
                spec.version.clone()
            } else {
                process.ready().version.clone()
            };
            Ok((Box::new(PluginText::new(process, &spec)) as Box<dyn OcrEngine>, version))
        },
    )
}

#[cfg(windows)]
fn phase_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_PHASE_FILE").map(PathBuf::from)
}

#[cfg(windows)]
fn identity_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_BACKEND_FILE").map(PathBuf::from)
}

#[cfg(windows)]
fn env_duration(name: &str, default: u64, minimum: u64, maximum: u64) -> Duration {
    let value = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(minimum, maximum);
    Duration::from_millis(value)
}

#[cfg(windows)]
fn env_seconds(name: &str, default: u64, minimum: u64, maximum: u64) -> Duration {
    let value = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(minimum, maximum);
    Duration::from_secs(value)
}

#[cfg(not(windows))]
fn main() {}
