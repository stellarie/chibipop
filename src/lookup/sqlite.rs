//! Read-only mmap'd SQLite.

use crate::dict::gloss::GlossDoc;
use crate::lookup::model::{Dictionary, Entry, TermRow};
use crate::present::DictInfo;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

pub struct SqliteDictionary {
    conn: Connection,
    gloss: RefCell<GlossCache>,
}

/// Must match `dict::build::SCHEMA_VERSION`.
const EXPECTED_SCHEMA_VERSION: i64 = 3;

/// Parsed trees the cache keeps.
///
/// A hover renders at most `MAX_RESULTS` = 10 entries, so this holds roughly
/// the last 25 hovers - enough that a dwell re-check, a drill-down, and a
/// collapsed-row swap all reparse nothing, and small enough that the retained
/// heap stays in the low megabytes. `examples/gloss_doc_alloc.rs` measures
/// what one cached entry costs.
const GLOSS_CACHE_ENTRIES: usize = 256;

/// The parsed-tree cache the spec asks for: the record stores raw glossary
/// JSON and the tree is parsed per hover, so a parser fix ships as a patch
/// rather than as a dictionary rebuild.
///
/// Insertion order, not recency. The access pattern is a sliding window of
/// recent hovers, so the two orders very nearly coincide, and FIFO costs one
/// push per miss instead of a touch per hit.
#[derive(Default)]
struct GlossCache {
    by_id: HashMap<i64, (i64, Arc<GlossDoc>)>,
    order: VecDeque<i64>,
}

impl GlossCache {
    fn get(&self, entry_id: i64) -> Option<(i64, Arc<GlossDoc>)> {
        self.by_id.get(&entry_id).map(|(dict_id, doc)| (*dict_id, Arc::clone(doc)))
    }

    fn put(&mut self, entry_id: i64, dict_id: i64, doc: Arc<GlossDoc>) {
        if self.by_id.insert(entry_id, (dict_id, doc)).is_none() {
            self.order.push_back(entry_id);
            while self.order.len() > GLOSS_CACHE_ENTRIES {
                if let Some(evicted) = self.order.pop_front() {
                    self.by_id.remove(&evicted);
                }
            }
        }
    }
}

impl SqliteDictionary {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening dictionary {}", path.display()))?;
        // 256MB window; OS pages it.
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;

        let found_version: Option<String> = conn
            .query_row("SELECT v FROM meta WHERE k = 'schema_version'", [], |r| {
                r.get(0)
            })
            .optional()
            .with_context(|| {
                format!("reading meta.schema_version from {}", path.display())
            })?;
        let is_current = found_version.as_deref().and_then(|v| v.parse::<i64>().ok())
            == Some(EXPECTED_SCHEMA_VERSION);
        if !is_current {
            let found_display = found_version.as_deref().unwrap_or("<missing>");
            anyhow::bail!(
                "{}: schema_version is {found_display}, but this build of \
                 chibipop expects {EXPECTED_SCHEMA_VERSION} - rebuild the \
                 dictionary from the settings window",
                path.display()
            );
        }

        Ok(SqliteDictionary { conn, gloss: RefCell::new(GlossCache::default()) })
    }
}

impl Dictionary for SqliteDictionary {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT surface, written, reading, pos, freq, entry_id, dict_id \
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
                dict_id: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The parse is per hover, behind the cache, and the raw TEXT is borrowed
    /// out of SQLite rather than copied into a `String` first: the parser
    /// reads a `&str`, so the owned copy the old `Vec<Sense>` path made had
    /// nothing to do.
    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        let mut stmt = self
            .conn
            .prepare_cached("SELECT dict_id, glossary FROM entry WHERE entry_id = ?1")?;
        for &id in ids {
            if let Some((dict_id, doc)) = self.gloss.borrow().get(id) {
                out.push(Entry::new(id, dict_id, doc));
                continue;
            }
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                let dict_id: i64 = r.get(0)?;
                let raw = r
                    .get_ref(1)?
                    .as_str()
                    .with_context(|| format!("reading the glossary of entry {id}"))?;
                let doc = Arc::new(GlossDoc::parse(raw));
                self.gloss.borrow_mut().put(id, dict_id, Arc::clone(&doc));
                out.push(Entry::new(id, dict_id, doc));
            }
        }
        Ok(out)
    }

    fn dicts(&self) -> Result<Vec<DictInfo>> {
        let mut stmt =
            self.conn.prepare_cached("SELECT dict_id, name FROM dict ORDER BY dict_id")?;
        let rows = stmt.query_map([], |r| {
            Ok(DictInfo { dict_id: r.get(0)?, name: r.get(1)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::Dictionary;
    use std::path::{Path, PathBuf};

    /// Removes the file on drop.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Unique per process+test.
    fn fixture_path(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_sqlite_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Real schema, then seeds.
    fn seed_fixture_db(path: &Path, seed_sql: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE dict(dict_id INTEGER PRIMARY KEY, name TEXT, priority INTEGER);
             CREATE TABLE entry(entry_id INTEGER PRIMARY KEY, dict_id INTEGER, glossary TEXT);
             CREATE TABLE term(surface TEXT, written TEXT, reading TEXT, pos TEXT,
                               freq INTEGER, entry_id INTEGER, dict_id INTEGER);
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
             INSERT INTO entry VALUES (1,1,'[{\"type\":\"structured-content\",\"content\":[\
                 {\"tag\":\"span\",\"data\":{\"content\":\"part-of-speech-info\"},\
                  \"content\":\"v1\"},\
                 {\"tag\":\"div\",\"content\":\"to eat\"}]}]');
             INSERT INTO term VALUES ('食べる','食べる','たべる','v1',500,1,1);
             INSERT INTO meta VALUES ('schema_version','3');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let rows = d.terms_for("食べる").unwrap();
        assert_eq!(1, rows.len());
        assert_eq!("v1", rows[0].pos);
        assert_eq!(Some(500), rows[0].freq);
        assert_eq!(1, rows[0].dict_id);

        let entries = d.entries(&[1]).unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(vec!["to eat".to_string()], entries[0].glosses());
        assert_eq!(vec!["v1".to_string()], entries[0].pos);

        assert!(d.terms_for("いぬ").unwrap().is_empty());
        assert!(d.entries(&[]).unwrap().is_empty());
    }

    /// The cache is what makes per-hover parsing affordable, so a second
    /// read must hand back the same parse rather than a second one.
    #[test]
    fn a_second_read_of_one_entry_reuses_the_parsed_tree() {
        let path = fixture_path("a_second_read_of_one_entry_reuses_the_parsed_tree");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO entry VALUES (1,1,'[\"to eat\"]');
             INSERT INTO meta VALUES ('schema_version','3');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let first = d.entries(&[1]).unwrap();
        let second = d.entries(&[1]).unwrap();
        assert!(
            Arc::ptr_eq(&first[0].gloss, &second[0].gloss),
            "the second hover on one entry must not reparse it"
        );
    }

    /// Common in the real data.
    #[test]
    fn nullable_columns_come_back_as_none() {
        let path = fixture_path("nullable_columns_come_back_as_none");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (2,'d',0);
             INSERT INTO entry VALUES (2,2,'[\"very\"]');
             INSERT INTO term VALUES ('とても',NULL,'とても','',NULL,2,2);
             INSERT INTO meta VALUES ('schema_version','3');",
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
        assert_eq!(2, rows[0].dict_id);
    }

    /// A version-2 database is the one every existing user has, and it must
    /// fail loudly with the rebuild message rather than mis-reading the
    /// renamed column.
    #[test]
    fn opening_a_version_two_database_fails_with_the_rebuild_message() {
        let path = fixture_path("opening_a_version_two_database_fails");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','2');");

        // No Debug on the Ok type.
        let err = SqliteDictionary::open(&path)
            .err()
            .expect("opening a version-2 database should fail");
        let msg = err.to_string();
        assert!(msg.contains("schema_version is 2"), "{msg}");
        assert!(msg.contains("expects 3"), "{msg}");
        assert!(msg.to_lowercase().contains("rebuild"), "{msg}");
    }

    /// Must fail loudly.
    #[test]
    fn open_fails_when_schema_version_does_not_match() {
        let path = fixture_path("open_fails_when_schema_version_does_not_match");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','1');");


        // No Debug on the Ok type.
        let err = SqliteDictionary::open(&path)
            .err()
            .expect("opening a dictionary with the wrong schema_version should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("schema_version is 1"),
            "error should name the version found in the file: {msg}"
        );
        assert!(
            msg.contains(&format!("expects {EXPECTED_SCHEMA_VERSION}")),
            "error should name the version this build expects: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("rebuild"),
            "error should tell the reader to rebuild the dictionary: {msg}"
        );
    }
}
