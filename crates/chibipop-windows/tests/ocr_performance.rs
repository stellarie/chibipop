#![cfg(windows)]

#[allow(dead_code)]
#[path = "../../ocr-performance/mod.rs"]
mod monitor;

use anyhow::{bail, Context, Result};
use chibipop::text::OcrEngine;
use chibipop_windows::plugin::{host, manifest, text::PluginText};
use chibipop_windows::text::ocr::WinrtOcr;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[test]
#[ignore = "Runs installed OCR engines for performance measurements"]
fn fixed_pixels_engine_latency() -> Result<()> {
    let fixture = monitor::load_fixture(&fixture_path())?;
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
    monitor::write_child_report(&report)
}

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
        },
    )
}

fn phase_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_PHASE_FILE").map(PathBuf::from)
}

fn identity_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_BACKEND_FILE").map(PathBuf::from)
}
