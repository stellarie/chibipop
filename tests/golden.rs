//! Golden corpus: (input, expected headword) against the real dictionary.
//!
//! No dictionary is committed here and CI builds none (ADR-0007 keeps the
//! Linux job headless, and a 238 MB build is not a CI step), so this skips
//! when it cannot find one. It looks where the *product* keeps its
//! dictionary rather than at a repo path nothing writes, best rung first:
//!
//! 1. `$CHIBIPOP_GOLDEN_DB`, for a library kept somewhere else entirely.
//! 2. `$XDG_DATA_HOME/chibipop/chibipop.sqlite` (`~/.local/share` when
//!    that is unset): the Linux daemon's `data_dir`.
//! 3. `<repo>/data/chibipop.sqlite`: where the Windows bin's default
//!    `--out` lands in a cargo tree.
//!
//! Rung 2 is spelled out here instead of called: the XDG resolver belongs
//! to `chibipop-linux` (`crates/chibipop-linux/src/paths.rs`) and that
//! crate depends on this one, so core's own tests cannot name it without
//! a dependency cycle. Rung 3 is what `chibipop::paths::data_file` answers
//! for a binary in `target/<profile>/` (`crates/chibipop-windows/src/entry.rs`);
//! a test binary sits a level down in `deps/`, so asking it directly would
//! probe `target/debug/deps/data/`, which nothing writes.
//!
//! The corpus deliberately does not pin a named dictionary build. Pinning
//! one would send it straight back to skipping everywhere, which is the
//! whole of ticket 17; instead every run prints the library that answered,
//! so a failure is attributable to that build. A skip names every path it
//! looked at, because an uncheckable skip is how this file spent its life
//! reporting `ok` without running.

use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::Dictionary;
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

/// Point the corpus at a library outside the default layout.
const DB_ENV: &str = "CHIBIPOP_GOLDEN_DB";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every place a real library could be, best rung first.
fn candidates() -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var_os(DB_ENV) {
        return vec![PathBuf::from(explicit)];
    }
    // A relative `$XDG_DATA_HOME` is invalid per the basedir spec and is
    // ignored, the same way the daemon's `paths::xdg` ignores it.
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
    // Present but unusable is a failure, not a skip: only the genuine
    // absence of a library is allowed to be quiet.
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
