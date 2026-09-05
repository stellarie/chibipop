//! The golden corpus checks `(input, expected headword)` against a real Dictionary.
//! The corpus also pins the Inflection chain of the matched hit.
//!
//! CI does not build or commit a Dictionary here. The Linux job stays headless,
//! and a 238 MB build is not a CI step. The test skips when it finds no Dictionary.
//! The test checks the product paths in this order:
//!
//! 1. `$CHIBIPOP_GOLDEN_DB`, for a Dictionary stored elsewhere.
//! 2. `$XDG_DATA_HOME/chibipop/chibipop.sqlite` (`~/.local/share` when
//!    unset), which is the Linux daemon's `data_dir`.
//! 3. `<repo>/data/chibipop.sqlite`, where the Windows bin's default `--out`
//!    lands in a cargo tree.
//!
//! This test lists rung 2 here instead of a call to the XDG resolver.
//! The resolver belongs to `chibipop-linux` (`crates/chibipop-linux/src/paths.rs`).
//! That crate depends on this one, so core tests cannot name it without
//! a dependency cycle.
//! Rung 3 uses the path from `chibipop::paths::data_file` for a binary in
//! `target/<profile>/` (`crates/chibipop-windows/src/entry.rs`).
//! A test binary sits one level below in `deps/`, so a direct query inspects
//! `target/debug/deps/data/`, which no process writes.
//!
//! The corpus does not pin a named Dictionary build.
//! A pin makes this test skip on most systems.
//! Each run prints the Dictionary that answers, so a failure names that build.
//! A skip prints every path that the test checks.
//! This prevents a silent `ok` result when the test did not run.

use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use std::path::PathBuf;

const CASES: &[(&str, &str, &[&str])] = &[
    ("食べる", "食べる", &[]),
    (
        "食べさせられた",
        "食べる",
        &["causative", "passive/potential", "past"],
    ),
    ("行かなかった", "行く", &["negative", "past"]),
    ("面白くない", "面白い", &["negative"]),
    ("見ている", "見る", &["teiru"]),
    ("日本語", "日本語", &[]),
    ("学校に行く", "学校", &[]),
    ("読みました", "読む", &["past polite"]),
    ("小さかった", "小さい", &["past"]),
    ("してしまった", "する", &["finish/completely/end up", "past"]),
];

/// Set the path to a Dictionary outside the default layout.
const DB_ENV: &str = "CHIBIPOP_GOLDEN_DB";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Return every path where a real Dictionary can exist, in priority order.
fn candidates() -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var_os(DB_ENV) {
        return vec![PathBuf::from(explicit)];
    }
    // Reject a relative `$XDG_DATA_HOME` under the basedir spec.
    // The daemon's `paths::xdg` applies the same rule.
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        });
    let mut out = Vec::new();
    out.extend(data_home.map(|dir| dir.join("chibipop/chibipop.sqlite")));
    out.push(root().join("data/chibipop.sqlite"));
    out
}

#[test]
fn golden_corpus() {
    let looked = candidates();
    let Some(db) = looked.iter().find(|db| db.exists()) else {
        let looked: Vec<String> =
            looked.iter().map(|db| db.display().to_string()).collect();
        eprintln!("skipping golden_corpus: no dictionary at {}", looked.join(", "));
        return;
    };

    let dict = SqliteDictionary::open(db).unwrap();
    // Treat a present but unusable file as a failure, not a skip.
    // Only an absent Dictionary can pass without output.
    let names: Vec<String> =
        dict.dicts().unwrap().into_iter().map(|d| d.name).collect();
    assert!(
        !names.is_empty(),
        "{}: a dictionary file with no dictionaries in it",
        db.display()
    );
    eprintln!(
        "golden_corpus: {} cases against {} ({})",
        CASES.len(),
        db.display(),
        names.join(", ")
    );

    let engine = LookupEngine::new(Deconjugator::new(
        load_rules(&root().join("data/deconjugator.json")).unwrap(),
    ));

    let mut failures = Vec::new();
    for (input, expected, chain) in CASES {
        let hits = engine.run(&dict, input).unwrap();
        let Some(hit) = hits.iter().take(3).find(|h| {
            h.written.as_deref() == Some(*expected)
                || h.reading.as_deref() == Some(*expected)
        }) else {
            let top: Vec<String> = hits
                .iter()
                .take(3)
                .map(|h| {
                    h.written.clone().or(h.reading.clone()).unwrap_or_default()
                })
                .collect();
            failures.push(format!(
                "{input}: expected {expected} in top 3, got {top:?}"
            ));
            continue;
        };
        let got = chibipop::present::inflection_chain(&hit.process);
        if got != *chain {
            failures.push(format!(
                "{input}: expected inflection chain {chain:?}, got {got:?} from raw process {:?}",
                hit.process
            ));
        }
    }
    assert!(failures.is_empty(), "golden failures:\n{}", failures.join("\n"));
}
