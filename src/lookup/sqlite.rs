//! Read-only, memory-mapped SQLite implementation of `Dictionary`.

use crate::lookup::model::{Dictionary, Entry, Sense, TermRow};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub struct SqliteDictionary {
    conn: Connection,
}

impl SqliteDictionary {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening dictionary {}", path.display()))?;
        // 256MB mmap window: the OS pages what is touched, so resident
        // memory stays near zero.
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        Ok(SqliteDictionary { conn })
    }
}

impl Dictionary for SqliteDictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT surface, written, reading, pos, freq, entry_id \
             FROM term WHERE surface = ?1",
        )?;
        let rows = stmt.query_map([surface], |r| {
            Ok(TermRow {
                surface: r.get(0)?,
                written: r.get(1)?,
                reading: r.get(2)?,
                pos: r.get(3)?,
                freq: r.get(4)?,
                entry_id: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        let mut stmt = self.conn.prepare_cached(
            "SELECT entry_id, dict_id, senses FROM entry WHERE entry_id = ?1",
        )?;
        for id in ids {
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                let senses_json: String = r.get(2)?;
                let senses: Vec<Sense> = serde_json::from_str(&senses_json)
                    .with_context(|| format!("parsing senses for entry {id}"))?;
                out.push(Entry {
                    entry_id: r.get(0)?,
                    dict_id: r.get(1)?,
                    senses,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::Dictionary;
    use std::path::{Path, PathBuf};

    /// Removes the wrapped fixture file when dropped — including when a test
    /// panics mid-assertion (Rust unwinds through `Drop` on panic), so a
    /// failing test doesn't leave a `.sqlite` file behind either.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Unique fixture path for `test_name`: the process id plus the test
    /// name means two concurrent `cargo test` runs, or two fixture-backed
    /// tests in this module, never contend for the same file. That matters
    /// on Windows, where a handle left open by one process/test can block
    /// another's create or delete on the same path.
    fn fixture_path(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_sqlite_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Creates the production `dict`/`entry`/`term`/`meta` schema at `path`,
    /// then runs `seed_sql` (plain `INSERT`s) to populate it.
    fn seed_fixture_db(path: &Path, seed_sql: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE dict(dict_id INTEGER PRIMARY KEY, name TEXT, priority INTEGER);
             CREATE TABLE entry(entry_id INTEGER PRIMARY KEY, dict_id INTEGER, senses TEXT);
             CREATE TABLE term(surface TEXT, written TEXT, reading TEXT, pos TEXT,
                               freq INTEGER, entry_id INTEGER);
             CREATE TABLE meta(k TEXT PRIMARY KEY, v TEXT);
             CREATE INDEX idx_term_surface ON term(surface);",
        )
        .unwrap();
        conn.execute_batch(seed_sql).unwrap();
    }

    #[test]
    fn reads_terms_and_entries() {
        let path = fixture_path("reads_terms_and_entries");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO entry VALUES (1,1,'[{\"glosses\":[\"to eat\"],\"pos\":[\"v1\"],\"misc\":[]}]');
             INSERT INTO term VALUES ('食べる','食べる','たべる','v1',500,1);",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let rows = d.terms_for("食べる").unwrap();
        assert_eq!(1, rows.len());
        assert_eq!("v1", rows[0].pos);
        assert_eq!(Some(500), rows[0].freq);

        let entries = d.entries(&[1]).unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(vec!["to eat".to_string()], entries[0].senses[0].glosses);

        assert!(d.terms_for("いぬ").unwrap().is_empty());
        assert!(d.entries(&[]).unwrap().is_empty());
    }

    /// Real-world shape check: `written IS NULL` on 275,818/1,261,454 term
    /// rows (kana-only headwords) and `freq IS NULL` on 589,498/1,261,454
    /// (unranked entries) — 21.9% and 46.7% of the real dictionary
    /// respectively. Neither was exercised by `reads_terms_and_entries`,
    /// whose one row is fully populated. This also covers `pos = ''`
    /// (`pos` is `String`, not `Option<String>`; empty means "part of
    /// speech unknown" and is extremely common in the real data), and
    /// confirms `reading`, `surface`, and `entry_id` still round-trip
    /// correctly alongside the two NULLs.
    #[test]
    fn nullable_columns_come_back_as_none() {
        let path = fixture_path("nullable_columns_come_back_as_none");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (2,'d',0);
             INSERT INTO entry VALUES (2,2,'[{\"glosses\":[\"very\"],\"pos\":[],\"misc\":[]}]');
             INSERT INTO term VALUES ('とても',NULL,'とても','',NULL,2);",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let rows = d.terms_for("とても").unwrap();
        assert_eq!(1, rows.len());
        assert_eq!("とても", rows[0].surface);
        assert_eq!(None, rows[0].written);
        assert_eq!(Some("とても".to_string()), rows[0].reading);
        assert_eq!("", rows[0].pos);
        assert_eq!(None, rows[0].freq);
        assert_eq!(2, rows[0].entry_id);
    }
}
