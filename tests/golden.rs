//! Golden corpus: (input, expected headword) against the real dictionary.
//!
//! Skipped when data/chibipop.sqlite is absent, so a fresh clone still
//! passes `cargo test`.

use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use std::path::PathBuf;

const CASES: &[(&str, &str)] = &[
    ("食べる", "食べる"),
    ("食べさせられた", "食べる"),
    ("行かなかった", "行く"),
    ("面白くない", "面白い"),
    ("見ている", "見る"),
    ("日本語", "日本語"),
    ("学校に行く", "学校"),
    ("読みました", "読む"),
    ("小さかった", "小さい"),
    ("してしまった", "する"),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn golden_corpus() {
    let db = root().join("data/chibipop.sqlite");
    if !db.exists() {
        eprintln!("SKIP golden_corpus: {} not built", db.display());
        return;
    }

    let dict = SqliteDictionary::open(&db).unwrap();
    let engine = LookupEngine::new(Deconjugator::new(
        load_rules(&root().join("data/deconjugator.json")).unwrap(),
    ));

    let mut failures = Vec::new();
    for (input, expected) in CASES {
        let hits = engine.run(&dict, input).unwrap();
        let found = hits.iter().take(3).any(|h| {
            h.written.as_deref() == Some(*expected)
                || h.reading.as_deref() == Some(*expected)
        });
        if !found {
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
        }
    }
    assert!(failures.is_empty(), "golden failures:\n{}", failures.join("\n"));
}
