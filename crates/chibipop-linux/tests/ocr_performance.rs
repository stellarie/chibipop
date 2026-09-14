#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../../ocr-performance/mod.rs"]
mod monitor;

use anyhow::{bail, Result};
use chibipop::text::OcrEngine;
use chibipop_linux::ocr::{models, MeikiOcr};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[test]
#[ignore = "Runs the installed meikiocr engine for performance measurements"]
fn fixed_pixels_engine_latency() -> Result<()> {
    let fixture = monitor::load_fixture(&fixture_path())?;
    let requested = std::env::var("CHIBIPOP_OCR_PERF_BACKEND")
        .unwrap_or_else(|_| "meikiocr".to_string());
    if requested != "meikiocr" && requested != "all" {
        bail!("unsupported OCR performance backend: {requested}");
    }
    let report = json!({
        "schema": "chibipop-ocr-performance/v1",
        "mode": "report-only",
        "fixture": fixture.metadata,
        "backend_results": [run_meikiocr(&fixture)],
    });
    monitor::write_child_report(&report)
}

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

fn run_meikiocr(fixture: &monitor::Fixture) -> Value {
    monitor::run_benchmark_with_fixture(
        monitor::BenchmarkOptions {
            backend_id: "meikiocr".to_string(),
            version: "bundled".to_string(),
            language: "ja".to_string(),
            identity: json!({
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
            }),
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

fn phase_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_PHASE_FILE").map(PathBuf::from)
}

fn identity_file() -> Option<PathBuf> {
    std::env::var_os("CHIBIPOP_OCR_PERF_BACKEND_FILE").map(PathBuf::from)
}
