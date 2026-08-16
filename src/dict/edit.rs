//! Editing a live database.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::Path;

/// Base id for an empty table.
const FIRST_ID: i64 = 1;

/// Next free entry.entry_id.
pub fn next_entry_id(conn: &Connection) -> Result<i64> {
    next_id(conn, "SELECT MAX(entry_id) FROM entry")
}

/// Next free dict.dict_id.
pub fn next_dict_id(conn: &Connection) -> Result<i64> {
    next_id(conn, "SELECT MAX(dict_id) FROM dict")
}

/// MAX + 1; NULL means empty.
fn next_id(conn: &Connection, sql: &str) -> Result<i64> {
    let highest: Option<i64> =
        conn.query_row(sql, [], |r| r.get(0)).with_context(|| format!("running {sql}"))?;
    match highest {
        None => Ok(FIRST_ID),
        Some(id) => id.checked_add(1).with_context(|| format!("id space is full at {id}: {sql}")),
    }
}

/// Rows one removal deleted.
#[derive(Debug)]
pub struct Removed {
    pub dict_id: i64,
    pub dicts: usize,
    pub entries: usize,
    pub terms: usize,
    pub sources: usize,
}

/// Deletes one dictionary.
///
/// Absent ids are a no-op.
pub fn remove_dictionary(conn: &mut Connection, dict_id: i64, archive: &Path) -> Result<Removed> {
    let tx = conn.transaction().context("opening the removal transaction")?;
    crate::dict::build::ensure_indexes(&tx)?;

    let terms = delete_rows(&tx, "term", dict_id)?;
    let entries = delete_rows(&tx, "entry", dict_id)?;
    let dicts = delete_rows(&tx, "dict", dict_id)?;
    let sources = if dicts == 0 { 0 } else { forget_source(&tx, archive)? };
    refresh_stats(&tx)?;

    tx.commit().context("committing the removal")?;
    Ok(Removed { dict_id, dicts, entries, terms, sources })
}

/// Bounded ANALYZE of changes.
fn refresh_stats(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA analysis_limit = 400; PRAGMA optimize;")
        .context("refreshing the planner statistics")
}

/// Deletes one dict's rows.
fn delete_rows(conn: &Connection, table: &str, dict_id: i64) -> Result<usize> {
    let sql = format!("DELETE FROM {table} WHERE dict_id = ?1");
    conn.execute(&sql, params![dict_id]).with_context(|| format!("running {sql}"))
}

/// Drops one archive from meta.
fn forget_source(conn: &Connection, archive: &Path) -> Result<usize> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let raw: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
        .optional()
        .context("reading meta.source_hashes")?;
    let Some(raw) = raw else { return Ok(0) };

    let mut listed: Vec<Value> =
        serde_json::from_str(&raw).context("parsing meta.source_hashes")?;
    let before = listed.len();
    listed.retain(|rec| rec["name"].as_str() != Some(name));
    let dropped = before - listed.len();

    if dropped > 0 {
        let text = serde_json::to_string(&listed).context("writing meta.source_hashes")?;
        conn.execute("UPDATE meta SET v = ?1 WHERE k = 'source_hashes'", params![text])
            .context("updating meta.source_hashes")?;
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    fn fixture_db(test_name: &str) -> (Connection, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = TempDbGuard(out.clone());
        let terms = [fixture("terms.zip")];
        let freqs = [fixture("freq.zip")];
        crate::dict::build::build(&terms, &freqs, &out, &|_| {}).unwrap();
        (Connection::open(&out).unwrap(), guard)
    }

    fn add_dict(conn: &Connection, dict_id: i64) {
        conn.execute(
            "INSERT INTO dict (dict_id, name, priority) VALUES (?1, ?2, 0)",
            rusqlite::params![dict_id, format!("dict {dict_id}")],
        )
        .unwrap();
    }

    fn add_entry(conn: &Connection, entry_id: i64, dict_id: i64) {
        conn.execute(
            "INSERT INTO entry (entry_id, dict_id, senses) VALUES (?1, ?2, '[]')",
            rusqlite::params![entry_id, dict_id],
        )
        .unwrap();
    }

    fn populate(conn: &Connection) {
        add_dict(conn, 3);
        add_dict(conn, 7);
        add_entry(conn, 41, 7);
    }

    fn add_term(conn: &Connection, surface: &str, entry_id: i64, dict_id: i64) {
        conn.execute(
            "INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id)
             VALUES (?1, NULL, ?1, '', NULL, ?2, ?3)",
            rusqlite::params![surface, entry_id, dict_id],
        )
        .unwrap();
    }

    /// dict 2: 2 entries, 3 terms.
    fn add_second_dictionary(conn: &Connection) {
        add_dict(conn, 2);
        add_entry(conn, 4, 2);
        add_entry(conn, 5, 2);
        add_term(conn, "ねずみ", 4, 2);
        add_term(conn, "いぬ", 5, 2);
        add_term(conn, "うま", 5, 2);
    }

    fn ids(conn: &Connection, sql: &str) -> Vec<i64> {
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn rows(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(sql).unwrap();
        let width = stmt.column_count();
        let mapped = stmt
            .query_map([], |r| {
                let cells: Vec<String> =
                    (0..width).map(|i| format!("{:?}", r.get_ref_unwrap(i))).collect();
                Ok(cells.join("|"))
            })
            .unwrap();
        mapped.map(|r| r.unwrap()).collect()
    }

    /// Every byte of one dict.
    fn snapshot(conn: &Connection, dict_id: i64) -> Vec<String> {
        let mut all = rows(conn, &format!("SELECT * FROM dict WHERE dict_id = {dict_id}"));
        all.extend(rows(
            conn,
            &format!("SELECT * FROM entry WHERE dict_id = {dict_id} ORDER BY entry_id"),
        ));
        all.extend(rows(
            conn,
            &format!("SELECT rowid, * FROM term WHERE dict_id = {dict_id} ORDER BY rowid"),
        ));
        all
    }

    fn source_names(conn: &Connection) -> Vec<String> {
        let raw: String = conn
            .query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        parsed
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn index_present(conn: &Connection) -> bool {
        count(conn, "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_term_entry_id'") == 1
    }

    #[test]
    fn the_fixture_starts_where_the_builder_left_off() {
        let (conn, _guard) = fixture_db("fixture_baseline");
        assert_eq!(4, next_entry_id(&conn).unwrap());
        assert_eq!(2, next_dict_id(&conn).unwrap());
    }

    #[test]
    fn next_entry_id_is_one_above_the_highest_row() {
        let (conn, _guard) = fixture_db("entry_max_plus_one");
        populate(&conn);
        assert_eq!(42, next_entry_id(&conn).unwrap());
    }

    #[test]
    fn next_dict_id_is_one_above_the_highest_row() {
        let (conn, _guard) = fixture_db("dict_max_plus_one");
        populate(&conn);
        assert_eq!(8, next_dict_id(&conn).unwrap());
    }

    #[test]
    fn an_empty_table_reads_null_and_allocates_the_base() {
        let (conn, _guard) = fixture_db("empty_allocates_base");
        conn.execute_batch("DELETE FROM term; DELETE FROM entry; DELETE FROM dict;").unwrap();

        let max_entry: Option<i64> =
            conn.query_row("SELECT MAX(entry_id) FROM entry", [], |r| r.get(0)).unwrap();
        let max_dict: Option<i64> =
            conn.query_row("SELECT MAX(dict_id) FROM dict", [], |r| r.get(0)).unwrap();
        assert_eq!(None, max_entry, "MAX on an empty table is SQL NULL, not 0");
        assert_eq!(None, max_dict, "MAX on an empty table is SQL NULL, not 0");

        assert_eq!(1, next_entry_id(&conn).unwrap());
        assert_eq!(1, next_dict_id(&conn).unwrap());
    }

    #[test]
    fn a_gap_left_by_a_delete_is_not_reused() {
        let (conn, _guard) = fixture_db("gaps_are_not_reused");
        populate(&conn);
        conn.execute_batch(
            "DELETE FROM term WHERE entry_id = 2;
             DELETE FROM entry WHERE entry_id = 2;
             DELETE FROM dict WHERE dict_id = 3;",
        )
        .unwrap();

        let entries: Vec<i64> = ids(&conn, "SELECT entry_id FROM entry ORDER BY entry_id");
        let dicts: Vec<i64> = ids(&conn, "SELECT dict_id FROM dict ORDER BY dict_id");
        assert_eq!(vec![1, 3, 41], entries);
        assert_eq!(vec![1, 7], dicts);

        assert_eq!(42, next_entry_id(&conn).unwrap());
        assert_eq!(8, next_dict_id(&conn).unwrap());
    }

    #[test]
    fn deleting_the_highest_row_hands_its_id_back() {
        let (conn, _guard) = fixture_db("highest_id_is_recycled");
        populate(&conn);
        assert_eq!(8, next_dict_id(&conn).unwrap());

        conn.execute_batch(
            "DELETE FROM entry WHERE dict_id = 7;
             DELETE FROM dict WHERE dict_id = 7;",
        )
        .unwrap();

        assert_eq!(4, next_dict_id(&conn).unwrap());
    }

    #[test]
    fn an_exhausted_id_space_is_an_error_not_a_wrap() {
        let (conn, _guard) = fixture_db("exhausted_id_space");
        add_entry(&conn, i64::MAX, 1);

        let err = next_entry_id(&conn).unwrap_err();
        assert!(
            format!("{err:#}").contains("id space is full"),
            "expected an exhaustion error, got: {err:#}"
        );
    }

    fn term_stat(conn: &Connection) -> String {
        conn.query_row(
            "SELECT stat FROM sqlite_stat1 WHERE idx = 'idx_term_entry_id'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// n entries, 2n terms.
    fn add_bulk_dictionary(conn: &Connection, dict_id: i64, n: i64) {
        add_dict(conn, dict_id);
        let base = dict_id * 1_000_000;
        conn.execute_batch(&format!(
            "INSERT INTO entry (entry_id, dict_id, senses)
               WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i < {n})
               SELECT i + {base}, {dict_id}, '[]' FROM s;
             INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id)
               WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i < {})
               SELECT 'w' || i, NULL, 'r' || i, '', NULL,
                      ((i - 1) / 2) + 1 + {base}, {dict_id}
               FROM s;",
            n * 2
        ))
        .unwrap();
    }

    #[test]
    fn removing_one_dictionary_leaves_the_other_exactly_intact() {
        let (mut conn, _guard) = fixture_db("survivor_is_intact");
        add_second_dictionary(&conn);
        let before = snapshot(&conn, 1);
        assert_eq!(9, before.len(), "1 dict + 3 entry + 5 term rows");

        remove_dictionary(&mut conn, 2, Path::new("other.zip")).unwrap();

        assert_eq!(before, snapshot(&conn, 1), "the survivor's bytes must not move");
        assert_eq!(3, count(&conn, "SELECT COUNT(*) FROM entry"));
        assert_eq!(5, count(&conn, "SELECT COUNT(*) FROM term"));
        assert_eq!(vec![1], ids(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"));
    }

    #[test]
    fn a_removal_deletes_the_terms_the_entries_and_the_dict_row() {
        let (mut conn, _guard) = fixture_db("removal_deletes_all_three");
        add_second_dictionary(&conn);
        let survivor = snapshot(&conn, 2);

        let gone = remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        assert_eq!(1, gone.dict_id);
        assert_eq!(1, gone.dicts);
        assert_eq!(3, gone.entries);
        assert_eq!(5, gone.terms);
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM term WHERE dict_id = 1"));
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM entry WHERE dict_id = 1"));
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM dict WHERE dict_id = 1"));
        assert_eq!(survivor, snapshot(&conn, 2), "dict 2 must be untouched");
    }

    #[test]
    fn the_entry_id_index_exists_before_the_first_delete() {
        let (mut conn, _guard) = fixture_db("index_precedes_the_delete");
        conn.execute_batch("DROP INDEX idx_term_entry_id;").unwrap();
        assert!(!index_present(&conn), "a pre-v0.8.0 file has no entry_id index");

        conn.execute_batch(
            "CREATE TEMP TABLE probe (present INTEGER);
             CREATE TEMP TRIGGER probe_delete BEFORE DELETE ON term BEGIN
               INSERT INTO probe (present)
                 SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_term_entry_id';
             END;",
        )
        .unwrap();

        remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        let seen = ids(&conn, "SELECT present FROM probe");
        assert_eq!(vec![1; 5], seen, "the index must exist as each term row is deleted");
        assert!(index_present(&conn));
    }

    #[test]
    fn removing_a_dict_id_that_does_not_exist_is_a_clean_no_op() {
        let (mut conn, _guard) = fixture_db("absent_dict_is_a_no_op");
        add_second_dictionary(&conn);
        let one = snapshot(&conn, 1);
        let two = snapshot(&conn, 2);
        let sources = source_names(&conn);

        let gone = remove_dictionary(&mut conn, 99, Path::new("terms.zip"))
            .expect("an absent dictionary is not an error");

        assert_eq!(0, gone.dicts);
        assert_eq!(0, gone.entries);
        assert_eq!(0, gone.terms);
        assert_eq!(0, gone.sources);
        assert_eq!(one, snapshot(&conn, 1));
        assert_eq!(two, snapshot(&conn, 2));
        assert_eq!(sources, source_names(&conn), "a no-op must not edit meta");
    }

    #[test]
    fn the_removed_archive_is_dropped_from_source_hashes() {
        let (mut conn, _guard) = fixture_db("source_hashes_forgets");
        assert_eq!(vec!["terms.zip", "freq.zip"], source_names(&conn));

        let gone = remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        assert_eq!(1, gone.sources);
        assert_eq!(vec!["freq.zip"], source_names(&conn), "only the removed archive goes");
    }

    #[test]
    fn a_full_archive_path_matches_the_recorded_file_name() {
        let (mut conn, _guard) = fixture_db("full_path_still_matches");
        let path = Path::new("C:\\Users\\Stella\\chibipop\\library\\terms.zip");

        let gone = remove_dictionary(&mut conn, 1, path).unwrap();

        assert_eq!(1, gone.sources, "source_hashes stores the file name, not the path");
        assert_eq!(vec!["freq.zip"], source_names(&conn));
    }

    #[test]
    fn an_unlisted_archive_leaves_source_hashes_alone() {
        let (mut conn, _guard) = fixture_db("unlisted_archive");
        add_second_dictionary(&conn);

        let gone = remove_dictionary(&mut conn, 2, Path::new("never-built-from.zip")).unwrap();

        assert_eq!(1, gone.dicts);
        assert_eq!(0, gone.sources);
        assert_eq!(vec!["terms.zip", "freq.zip"], source_names(&conn));
    }

    #[test]
    fn a_surviving_source_record_keeps_every_field_it_had() {
        let (mut conn, _guard) = fixture_db("unknown_fields_survive");
        let legacy = r#"[{"name": "terms.zip"},
             {"name": "freq.zip", "bytes": 7, "note": "keep me", "sha256": "ab"}]"#;
        conn.execute("UPDATE meta SET v = ?1 WHERE k = 'source_hashes'", params![legacy])
            .unwrap();

        remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        let raw: String = conn
            .query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
            .unwrap();
        let kept: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(1, kept.as_array().unwrap().len());
        assert_eq!(Some("keep me"), kept[0]["note"].as_str());
        assert_eq!(Some(7), kept[0]["bytes"].as_i64());
    }

    #[test]
    fn a_removal_refreshes_grossly_stale_planner_statistics() {
        let (mut conn, _guard) = fixture_db("stats_are_refreshed");
        add_bulk_dictionary(&conn, 2, 3_000);
        assert_eq!("5 2", term_stat(&conn), "the build's ANALYZE saw 5 term rows");

        remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        let after = term_stat(&conn);
        let rows: i64 = after.split(' ').next().unwrap().parse().unwrap();
        assert_eq!(6_000, count(&conn, "SELECT COUNT(*) FROM term"));
        assert!(rows > 1_000, "an edit must not leave the planner reading {after}");
    }

    #[test]
    fn a_failure_part_way_through_rolls_the_whole_removal_back() {
        let (mut conn, _guard) = fixture_db("failure_rolls_back");
        add_second_dictionary(&conn);
        assert_eq!(1, count(&conn, "PRAGMA foreign_keys"), "the rollback needs foreign keys");
        add_term(&conn, "orphan", 1, 2);
        let before = snapshot(&conn, 1);
        let sources = source_names(&conn);

        let err = remove_dictionary(&mut conn, 1, Path::new("terms.zip"))
            .expect_err("a dangling term row must abort the removal");
        assert!(format!("{err:#}").to_lowercase().contains("foreign key"), "got: {err:#}");

        assert_eq!(before, snapshot(&conn, 1), "the term deletes must roll back too");
        assert_eq!(sources, source_names(&conn));
    }
}
