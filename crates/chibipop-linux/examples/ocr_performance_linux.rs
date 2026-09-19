// Native Linux wrapper.
#[cfg(target_os = "linux")]
#[path = "../../ocr-performance/mod.rs"]
mod monitor;

#[cfg(target_os = "linux")]
use anyhow::{bail, Context, Result};
#[cfg(target_os = "linux")]
use chibipop::text::OcrEngine;
#[cfg(target_os = "linux")]
use chibipop_linux::ocr::{models, MeikiOcr};
#[cfg(target_os = "linux")]
use serde_json::{json, Value};
#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn child_main() -> Result<()> {
    let backend = std::env::args().nth(2).context("missing OCR backend")?;
    if backend != "meikiocr" {
        bail!("unsupported OCR performance backend: {backend}");
    }
    let fixture = monitor::load_fixture(&fixture_path())?;
    let result = run_meikiocr(&fixture);
    let report = json!({
        "schema": "chibipop-ocr-performance/v1",
        "mode": "report-only",
        "fixture": fixture.metadata,
        "backend_results": [result],
    });
    monitor::write_child_report(&report)
}

#[cfg(target_os = "linux")]
fn report_main() -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_path = fixture_path();
    let child_program = std::env::current_exe().context("locating the OCR report executable")?;
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
        backends: vec![monitor::BackendPlan {
            id: "meikiocr".to_string(),
            child_backend: "meikiocr".to_string(),
            fallback_identity: meiki_identity(),
            environment: BTreeMap::new(),
            // The models are committed, so this runner must measure them.
            required: true,
        }],
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

#[cfg(target_os = "linux")]
fn fixture_path() -> PathBuf {
    std::env::var_os("CHIBIPOP_OCR_PERF_FIXTURE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("chibipop-windows")
                .join("tests")
                .join("fixtures")
                .join(monitor::FIXTURE_ID)
        })
}

#[cfg(target_os = "linux")]
fn meiki_identity() -> Value {
    json!({
        "id": "meikiocr",
        "version": "bundled",
        "language": "ja",
        "geometry": true,
        "model_hashes": {
            models::DETECT.0: models::DETECT.1,
            models::RECOGNISE.0: models::RECOGNISE.1,
            models::RECOGNISE_VERTICAL.0: models::RECOGNISE_VERTICAL.1,
        },
        "plugin_hashes": {},
        "config_sha256": null,
        "model_asset_count": models::ALL.len(),
        "identity_complete": true,
        "identity_incomplete_reason": null,
        "thread_settings": {
            "runtime": "ort",
            "intra_op_allow_spinning": "0",
            "inter_op_allow_spinning": "0",
        },
        "scales": [1, 2],
    })
}

#[cfg(target_os = "linux")]
fn run_meikiocr(fixture: &monitor::Fixture) -> Value {
    monitor::run_benchmark_with_fixture(
        monitor::BenchmarkOptions {
            backend_id: "meikiocr".to_string(),
            version: "bundled".to_string(),
            language: "ja".to_string(),
            identity: meiki_identity(),
            fixture: Value::Null,
            phase_file: phase_file(),
            identity_file: identity_file(),
        },
        fixture,
        || {
            let dir = std::env::var_os("CHIBIPOP_MODEL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("models/meiki"));
            MeikiOcr::open(&dir)
                .map(|engine| (Box::new(engine) as Box<dyn OcrEngine>, "bundled".to_string()))
        },
    )
}

#[cfg(target_os = "linux")]
fn phase_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_PHASE_FILE").map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn identity_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_BACKEND_FILE").map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn env_duration(name: &str, default: u64, minimum: u64, maximum: u64) -> Duration {
    let value = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(minimum, maximum);
    Duration::from_millis(value)
}

#[cfg(target_os = "linux")]
fn env_seconds(name: &str, default: u64, minimum: u64, maximum: u64) -> Duration {
    let value = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(minimum, maximum);
    Duration::from_secs(value)
}

#[cfg(not(target_os = "linux"))]
fn main() {}
