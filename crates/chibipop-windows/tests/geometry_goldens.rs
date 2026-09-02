//! This test checks DirectWrite geometry-snapshot goldens.
//!
//! Each fixture has one JSON golden under `tests/goldens/geometry/`.
//! The test compares snapshots with EXACT equality. It allows no tolerance.
//! DirectWrite metrics stay deterministic for a fixed font file.
//! The pinned tier0 runner image must reproduce the metrics bit-for-bit.
//! A mismatch names the fixture, the element, and the coordinate that moved.
//!
//! Set `CHIBIPOP_BLESS=1` to make each golden test write its golden.
//! This is the ONLY way that this file writes a golden.
//! A normal run never overwrites a golden.
//! If a golden is absent and bless is off, the test fails and tells the maintainer to bless it.
//!
//! Windows-only: this test calls DirectWrite.
//! Elsewhere, this file compiles to zero tests.
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

/// Keep each scalar under its dotted path so the diff can name the field.
/// Keep a marker for each empty container so a vanished section appears in the diff.
/// The marker names a whole section that vanishes.
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

/// The function maps `…elements.3.y` to the
/// golden's `…elements.3.text` path.
/// It names a moved coordinate by its
/// element, not only by its index.
///
/// The function walks upward until it finds
/// an owner. Widened fields nest.
/// For example,
/// `elements.3.measured.line_boxes.1.baseline`
/// has no text of its own.
/// The element three levels above it still
/// identifies the moved coordinate.
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

    // The test reports a field-level diff, not one blob.
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
        // Equal fields with different bytes show that
        // the serializer changed.
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
         commit the reviewed diff. Never widen this to a tolerance.",
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

#[test]
fn geometry_golden_pitch_single() {
    check("pitch_single");
}

#[test]
fn geometry_golden_pitch_multiple() {
    check("pitch_multiple");
}

#[test]
fn geometry_golden_pitch_sources() {
    check("pitch_sources");
}

/// Pin this fixture set to exactly sixteen
/// fixtures with one golden file each.
/// The first seven keep the original intent
/// unchanged.
/// The next six satisfy the widened schema
/// because text-only fixtures cannot fill
/// its fields.
/// The last three came with the card
/// header's pitch geometry.
/// These are the only fixtures that pin
/// that geometry.
#[test]
fn the_fixture_set_is_the_pinned_sixteen() {
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
            "pitch_single",
            "pitch_multiple",
            "pitch_sources",
        ],
        names
    );
    for f in fixtures() {
        assert!(!f.variants.is_empty(), "fixture '{}' has no variants", f.name);
    }
}

/// One fixture's claim about its captured
/// snapshot.
type Claim = (&'static str, fn(&Value) -> bool);

/// Each fixture tests the feature in its
/// name.
///
/// A "ruby run" fixture without a
/// [`RubyBox`] in its scene is not a
/// fixture.
/// Do not inspect the golden by eye to find
/// this error.
/// If the tree no longer matches its author's
/// intended parse, a bless run can record
/// geometry with absent content.
/// Each named feature has a predicate over
/// the captured scene.
/// The predicate uses the same capture as
/// the golden.
#[test]
fn every_new_fixture_carries_the_feature_it_is_named_for() {
    let want: &[Claim] = &[
        // Two spans share one line and use two
        // sizes. The old seam could not express
        // this case.
        ("styled_spans", |v| {
            elems(v).any(|e| {
                let spans = arr(e, "spans");
                spans.len() > 1
                    && spans.iter().any(|s| s["shift"].as_str() != Some("0.00"))
                    && spans.windows(2).any(|w| w[0]["size"] != w[1]["size"])
            })
        }),
        // A box must paint a fill or a border.
        // Empty space alone does not count.
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
        // Two markers share one element.
        // Two levels share the line between them.
        ("nested_list", |v| elems(v).any(|e| arr(e, "marker").len() > 1)),
        ("table_spans", |v| {
            elems(v).any(|e| e["kind"] == "Table")
                && elems(v).filter(|e| e["kind"] == "Cell").count() > 1
        }),
        ("ruby_run", |v| elems(v).any(|e| !arr(e, "ruby").is_empty())),
        // An asset has a media key when the store
        // supplies it.
        // The key shows that the code consulted the
        // store, not the node.
        ("inline_image", |v| {
            elems(v).any(|e| e["kind"] == "Image" && e["image"]["key"] != Value::Null)
        }),
        // Marked kana use a pitch element whose
        // high moras carry an overline.
        // A top border without a fill represents
        // this notation.
        // A fixture without this box does not draw
        // a pitch pattern.
        ("pitch_single", |v| {
            elems(v).any(|e| {
                e["kind"] == "Pitch"
                    && arr(e, "inline_boxes").iter().any(|b| {
                        b["style"]["border_style"]
                            .as_array()
                            .is_some_and(|s| s.first() == Some(&Value::from("solid")))
                    })
            })
        }),
        // Four pitch elements use four rows.
        ("pitch_multiple", |v| elems(v).filter(|e| e["kind"] == "Pitch").count() == 4),
        // The `split` variant draws two rows.
        // The `agreed` variant draws one row with
        // the names of four Dictionaries.
        // This capture shows if the code loses that
        // collapse.
        ("pitch_sources", |v| {
            let rows: Vec<&Value> = elems(v).filter(|e| e["kind"] == "Pitch").collect();
            rows.len() == 3
                && rows.iter().any(|e| {
                    e["text"].as_str().is_some_and(|t| t.matches('\u{b7}').count() == 3)
                })
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

/// Return every element from every variant in draw order.
/// The bin paints the elements in this order.
fn elems(snap: &Value) -> impl Iterator<Item = &Value> {
    snap["variants"]
        .as_object()
        .into_iter()
        .flat_map(|vs| vs.values())
        .filter_map(|v| v["elements"].as_array())
        .flatten()
}

/// Return an element's array field or an empty slice.
/// This lets callers inspect each field without a branch.
fn arr<'a>(elem: &'a Value, field: &str) -> &'a [Value] {
    elem[field].as_array().map_or(&[], Vec::as_slice)
}

/// Return each box on one element in paint order.
/// The bin paints the block box before the inline boxes.
fn boxes(elem: &Value) -> impl Iterator<Item = &Value> {
    let block = (elem["block_box"] != Value::Null).then(|| &elem["block_box"]);
    block.into_iter().chain(arr(elem, "inline_boxes"))
}
