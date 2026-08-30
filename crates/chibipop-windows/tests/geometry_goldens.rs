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

use chibipop_windows::ui::render::geometry::{fixtures, snapshot, to_json_text};
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
///
/// Walks *up* until it finds one,
/// because ADR-0013's fields nest:
/// `elements.3.measured.line_boxes.1.baseline`
/// has no text of its own and the
/// element three levels above it is
/// still what moved.
fn owner_text(path: &str, golden: &BTreeMap<String, String>) -> Option<String> {
    let (mut parent, field) = path.rsplit_once('.')?;
    if field == "text" || field == "kind" {
        return None;
    }
    loop {
        if let Some(text) = golden.get(&format!("{parent}.text")) {
            let kind = golden
                .get(&format!("{parent}.kind"))
                .cloned()
                .or_else(|| golden.get(&format!("{parent}.action")).cloned())
                .unwrap_or_default();
            return Some(format!("{kind} {text}"));
        }
        parent = parent.rsplit_once('.')?.0;
    }
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

#[test]
fn geometry_golden_styled_spans() {
    check("styled_spans");
}

#[test]
fn geometry_golden_bordered_pill() {
    check("bordered_pill");
}

#[test]
fn geometry_golden_nested_list() {
    check("nested_list");
}

#[test]
fn geometry_golden_table_spans() {
    check("table_spans");
}

#[test]
fn geometry_golden_ruby_run() {
    check("ruby_run");
}

#[test]
fn geometry_golden_inline_image() {
    check("inline_image");
}

/// The ADR-0011 fixture set, pinned:
/// exactly these thirteen, one golden
/// file each. The first seven are the
/// original set with its intent
/// unchanged; the last six are the
/// ones ADR-0013 requires, because the
/// widened schema has fields no
/// plain-string fixture can fill.
#[test]
fn the_fixture_set_is_the_thirteen_from_adr_0011() {
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
            "styled_spans",
            "bordered_pill",
            "nested_list",
            "table_spans",
            "ruby_run",
            "inline_image",
        ],
        names
    );
    for f in fixtures() {
        assert!(!f.variants.is_empty(), "fixture '{}' has no variants", f.name);
    }
}

/// One fixture's own claim about its
/// captured snapshot.
type Claim = (&'static str, fn(&Value) -> bool);

/// Each fixture actually exercises the
/// feature it is named for.
///
/// A "ruby run" fixture whose scene
/// carries no [`RubyRun`] is not a
/// fixture, and the way to notice is
/// not to read the golden by eye - a
/// tree that stopped parsing the way
/// its author meant would simply
/// bless quieter geometry. So each of
/// the six names a predicate over the
/// captured scene, and the capture is
/// the same one the golden holds.
#[test]
fn every_new_fixture_carries_the_feature_it_is_named_for() {
    let want: &[Claim] = &[
        // Two spans on one line, in
        // two different sizes: the one
        // thing the old seam could not
        // express.
        ("styled_spans", |v| {
            elems(v).any(|e| {
                let spans = arr(e, "spans");
                spans.len() > 1
                    && spans.iter().any(|s| s["shift"].as_str() != Some("0.00"))
                    && spans.windows(2).any(|w| w[0]["size"] != w[1]["size"])
            })
        }),
        // A box that paints: a fill or
        // a border, not merely spacing.
        ("bordered_pill", |v| {
            elems(v).any(|e| {
                boxes(e).any(|b| {
                    b["style"]["background"] != Value::Null
                        || b["style"]["border_style"]
                            .as_array()
                            .is_some_and(|s| s.iter().any(|e| e != "none"))
                })
            })
        }),
        // Two markers on one element:
        // two levels sharing the line
        // between them.
        ("nested_list", |v| elems(v).any(|e| arr(e, "marker").len() > 1)),
        ("table_spans", |v| {
            elems(v).any(|e| e["kind"] == "Table")
                && elems(v).filter(|e| e["kind"] == "Cell").count() > 1
        }),
        ("ruby_run", |v| elems(v).any(|e| !arr(e, "ruby").is_empty())),
        // An asset with a media key,
        // which is what says the store
        // was consulted rather than the
        // node believed.
        ("inline_image", |v| {
            elems(v).any(|e| e["kind"] == "Image" && e["image"]["key"] != Value::Null)
        }),
    ];

    for (name, holds) in want {
        let fixture = fixtures()
            .into_iter()
            .find(|f| f.name == *name)
            .unwrap_or_else(|| panic!("fixture '{name}' is not in geometry::fixtures()"));
        let snap =
            snapshot(&fixture).unwrap_or_else(|e| panic!("capturing '{name}' failed: {e:#}"));
        assert!(holds(&snap), "fixture '{name}' does not exercise what it is named for");
    }
}

/// Every element of every variant, in
/// draw order.
fn elems(snap: &Value) -> impl Iterator<Item = &Value> {
    snap["variants"]
        .as_object()
        .into_iter()
        .flat_map(|vs| vs.values())
        .filter_map(|v| v["elements"].as_array())
        .flatten()
}

/// One element's array field, or empty.
fn arr<'a>(elem: &'a Value, field: &str) -> &'a [Value] {
    elem[field].as_array().map_or(&[], Vec::as_slice)
}

/// Every box on one element, block
/// first, exactly as a bin paints them.
fn boxes(elem: &Value) -> impl Iterator<Item = &Value> {
    let block = (elem["block_box"] != Value::Null).then(|| &elem["block_box"]);
    block.into_iter().chain(arr(elem, "inline_boxes"))
}
