//! Editing a live database.

use anyhow::{Context, Result};
use rusqlite::Connection;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn ids(conn: &Connection, sql: &str) -> Vec<i64> {
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
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
}
