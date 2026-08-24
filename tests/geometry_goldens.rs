//! DirectWrite geometry-snapshot goldens (ADR-0011).
//!
//! One JSON golden per fixture under `tests/goldens/geometry/`,
//! compared with EXACT equality - no tolerance. DirectWrite metrics
//! are deterministic for a fixed font file, so tier0's pinned runner
//! image must reproduce them bit-for-bit; a mismatch names the fixture,
//! the element, and the coordinate that moved.
//!
//! `CHIBIPOP_BLESS=1` flips every test from assert to write: goldens
//! are rewritten and the run passes loudly. That is the ONLY way this
//! file writes; a normal run can never overwrite a golden. Missing
//! golden without bless = failure telling the maintainer to bless.
//!
//! Windows-only: capture calls DirectWrite. Elsewhere this whole file
//! compiles to zero tests.
#![cfg(windows)]

use chibipop::ui::render::geometry::{fixtures, snapshot, to_json_text};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
        .join("geometry")
}

fn blessing() -> bool {
    std::env::var("CHIBIPOP_BLESS").as_deref() == Ok("1")
}

/// Leaves only: every scalar under
/// its dotted path. Empty containers
/// keep a marker so vanishing whole
/// sections is a named diff too.
fn flatten(v: &Value, path: &str, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) if m.is_empty() => {
            out.insert(path.to_string(), "{}".to_string());
        }
        Value::Object(m) => {
            for (k, val) in m {
                let sub = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                flatten(val, &sub, out);
            }
        }
        Value::Array(a) if a.is_empty() => {
            out.insert(path.to_string(), "[]".to_string());
        }
        Value::Array(a) => {
            for (i, val) in a.iter().enumerate() {
                flatten(val, &format!("{path}.{i}"), out);
            }
        }
        leaf => {
            out.insert(path.to_string(), leaf.to_string());
        }
    }
}

/// "…elements.3.y" -> the golden's
/// "…elements.3.text", so a moved
/// coordinate is named by what it
/// belongs to, not just an index.
fn owner_text(path: &str, golden: &BTreeMap<String, String>) -> Option<String> {
    let (parent, field) = path.rsplit_once('.')?;
    if field == "text" || field == "kind" {
        return None;
    }
    let text = golden.get(&format!("{parent}.text"))?;
    let kind = golden
        .get(&format!("{parent}.kind"))
        .cloned()
        .or_else(|| golden.get(&format!("{parent}.action")).cloned())
        .unwrap_or_default();
    Some(format!("{kind} {text}"))
}

fn check(name: &str) {
    let fixture = fixtures()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("fixture '{name}' is not in geometry::fixtures()"));
    let snap = snapshot(&fixture)
        .unwrap_or_else(|e| panic!("capturing '{name}' failed: {e:#}"));
    let measured_text = to_json_text(&snap);
    let path = goldens_dir().join(format!("{name}.json"));

    if blessing() {
        fs::create_dir_all(goldens_dir()).expect("creating tests/goldens/geometry");
        fs::write(&path, &measured_text)
            .unwrap_or_else(|e| panic!("blessing {} failed: {e}", path.display()));
        eprintln!(
            "BLESSED {}: golden REWRITTEN, nothing compared. Review the diff and commit it.",
            path.display()
        );
        return;
    }

    let golden_text = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no golden for '{name}' at {}.\n\
             Bless it from a trusted tree: run the CI workflow_dispatch with bless=true \
             (or `CHIBIPOP_BLESS=1 cargo test --test geometry_goldens` on Windows), \
             review the JSON, and commit it. Normal runs never write goldens.",
            path.display()
        )
    });

    if golden_text == measured_text {
        return;
    }

    // Field-level diff, not a blob.
    let golden_v: Value = serde_json::from_str(&golden_text)
        .unwrap_or_else(|e| panic!("golden {} is not valid JSON: {e}", path.display()));
    let mut golden_flat = BTreeMap::new();
    let mut measured_flat = BTreeMap::new();
    flatten(&golden_v, "", &mut golden_flat);
    flatten(&snap, "", &mut measured_flat);

    let mut lines: Vec<String> = Vec::new();
    for (k, g) in &golden_flat {
        match measured_flat.get(k) {
            Some(m) if m == g => {}
            Some(m) => {
                let ctx = owner_text(k, &golden_flat)
                    .map(|t| format!("  [{t}]"))
                    .unwrap_or_default();
                lines.push(format!("  {k}: golden {g} -> measured {m}{ctx}"));
            }
            None => lines.push(format!("  {k}: golden {g} -> MISSING from capture")),
        }
    }
    for (k, m) in &measured_flat {
        if !golden_flat.contains_key(k) {
            lines.push(format!("  {k}: NEW in capture -> {m}"));
        }
    }

    let total = lines.len();
    if total == 0 {
        // Same fields, different bytes:
        // the serializer itself drifted.
        panic!(
            "geometry golden '{name}': every field matches but the serialized text differs - \
             the golden serializer changed. Re-bless deliberately or revert the change."
        );
    }
    const SHOWN: usize = 40;
    if lines.len() > SHOWN {
        lines.truncate(SHOWN);
        lines.push(format!("  ... and {} more differing fields", total - SHOWN));
    }
    panic!(
        "geometry golden '{name}' diverged ({total} field(s)):\n{}\n\
         If this layout change is INTENTIONAL, re-bless via the CI bless dispatch and \
         commit the reviewed diff (ADR-0011). Never widen this to a tolerance.",
        lines.join("\n")
    );
}

#[test]
fn geometry_golden_wrapping_heavy() {
    check("wrapping_heavy");
}

#[test]
fn geometry_golden_side_panel() {
    check("side_panel");
}

#[test]
fn geometry_golden_scrolled() {
    check("scrolled");
}

#[test]
fn geometry_golden_match_highlight() {
    check("match_highlight");
}

#[test]
fn geometry_golden_full_chrome() {
    check("full_chrome");
}

#[test]
fn geometry_golden_minimal_edge() {
    check("minimal_edge");
}

#[test]
fn geometry_golden_kitchen_sink() {
    check("kitchen_sink");
}

/// The ADR-0011 fixture set, pinned:
/// exactly these seven, one golden
/// file each.
#[test]
fn the_fixture_set_is_the_seven_from_adr_0011() {
    let names: Vec<&str> = fixtures().iter().map(|f| f.name).collect();
    assert_eq!(
        vec![
            "wrapping_heavy",
            "side_panel",
            "scrolled",
            "match_highlight",
            "full_chrome",
            "minimal_edge",
            "kitchen_sink",
        ],
        names
    );
    for f in fixtures() {
        assert!(!f.variants.is_empty(), "fixture '{}' has no variants", f.name);
    }
}
