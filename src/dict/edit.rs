//! Edits a live database and keeps its file.

use crate::dict::build::DICT_KEYED;
use crate::dict::frequency::{reduce, FreqTable};
use crate::dict::reindex;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Base id for an empty table.
const FIRST_ID: i64 = 1;

/// Returns the next free `entry.entry_id`.
pub fn next_entry_id(conn: &Connection) -> Result<i64> {
    next_id(conn, "SELECT MAX(entry_id) FROM entry")
}

/// Returns the next free `dict.dict_id`.
pub fn next_dict_id(conn: &Connection) -> Result<i64> {
    next_id(conn, "SELECT MAX(dict_id) FROM dict")
}

/// Returns MAX + 1. SQL NULL means that the table has no rows.
fn next_id(conn: &Connection, sql: &str) -> Result<i64> {
    let highest: Option<i64> =
        conn.query_row(sql, [], |r| r.get(0)).with_context(|| format!("running {sql}"))?;
    match highest {
        None => Ok(FIRST_ID),
        Some(id) => id.checked_add(1).with_context(|| format!("id space is full at {id}: {sql}")),
    }
}

/// Reports the rows that one addition inserted.
#[derive(Debug)]
pub struct Added {
    pub dict_id: i64,
    pub name: String,
    pub entries: i64,
    pub terms: i64,
    pub first_entry_id: i64,
}

/// Adds one Dictionary from an archive.
///
/// IDs use MAX + 1.
///
/// An archive can supply several Dictionary roles. An archive with only
/// frequency data adds no `term` rows. It still gets a `dict` row, so stored
/// Reported frequencies and Pitch patterns can use that id
/// (ARCHITECTURE.md#dictionary-and-lookup). The code does not reject an
/// archive because its name contains `Freq`.
pub fn add_dictionary(
    conn: &mut Connection,
    archive: &Path,
    freqs: &[PathBuf],
    on_progress: &dyn Fn(&str),
) -> Result<Added> {
    let sources = crate::dict::build::load_freqs(freqs)?;
    let tables: Vec<FreqTable> = sources.into_iter().map(|s| s.table).collect();

    let tx = conn.transaction().context("opening the addition transaction")?;
    // Reduce the new Dictionary with the strategy recorded in this database.
    // Use the caller's archive list, not the stored claims. The caller's list
    // defines the active frequency archives. [`reapply_frequencies`] reconciles
    // stored claims separately, and term-row insertion does not change frequency data.
    let strategy = reindex::recorded(&tx)?.strategy;
    let ranks = reduce(&tables, strategy);
    let dict_id = next_dict_id(&tx)?;
    let slot = crate::dict::build::Slot {
        dict_id,
        // Keep the priority relation that build-dict uses.
        priority: dict_id - 1,
        first_entry_id: next_entry_id(&tx)?,
    };
    let mut batches = crate::dict::build::Batches::new();
    let made = crate::dict::build::insert_archive(
        &tx,
        archive,
        &slot,
        &ranks,
        &mut batches,
        on_progress,
    )?;
    // `insert_archive` writes every bank before it returns.
    //
    // Store the archive's Pitch patterns under the same `dict_id`.
    // One archive can supply more than one role. The same id preserves both its
    // terms and its Pitch patterns in a combined archive.
    let table = crate::dict::pitch::load_pitch(archive)
        .with_context(|| format!("reading the pitch of {}", archive.display()))?;
    let accents = crate::dict::build::store_pitch(&tx, dict_id, &table)?;
    if accents > 0 {
        on_progress(&format!("pitch     [{}] {accents} accents", made.name));
    }
    record_source(&tx, archive)?;
    refresh_stats(&tx)?;

    tx.commit().context("committing the addition")?;
    Ok(Added {
        dict_id,
        name: made.name,
        entries: made.entries,
        terms: made.terms,
        first_entry_id: slot.first_entry_id,
    })
}

/// Reconciles stored Reported frequencies with the frequency archives, then
/// recomputes every Frequency rank.
///
/// Use this archive-driven entry point for an import or removal. `freqs` lists
/// every active frequency archive. A Dictionary that the list names keeps its
/// `dict` row and stored claims. A Dictionary that leaves the list loses its claims.
/// Preserve the recorded strategy. A settings change reads no archive and calls
/// [`crate::dict::reindex::reindex`] instead.
///
/// Returns the number of `term` rows that the pass restamped.
pub fn reapply_frequencies(
    conn: &mut Connection,
    freqs: &[PathBuf],
    on_progress: &dyn Fn(&str),
) -> Result<u64> {
    let sources = crate::dict::build::load_freqs(freqs)?;
    let tx = conn
        .transaction()
        .context("opening the frequency update transaction")?;

    let reduction = reindex::sync_reported(&tx, &sources)?;
    let processed = reindex::restamp_from_stored(&tx, &reduction, on_progress)?;

    refresh_stats(&tx)?;
    tx.commit().context("committing frequency updates")?;
    Ok(processed)
}

/// Records one archive in `meta`.
pub fn record_source(conn: &Connection, archive: &Path) -> Result<()> {
    let record = serde_json::to_value(crate::dict::build::source_hash(archive)?)
        .context("encoding the source record")?;
    let raw: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
        .optional()
        .context("reading meta.source_hashes")?;

    let mut listed: Vec<Value> = match raw {
        None => Vec::new(),
        Some(raw) => serde_json::from_str(&raw).context("parsing meta.source_hashes")?,
    };
    listed.retain(|rec| rec["name"] != record["name"]);
    listed.push(record);

    let text = serde_json::to_string(&listed).context("writing meta.source_hashes")?;
    conn.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('source_hashes', ?1)", params![text])
        .context("updating meta.source_hashes")?;
    Ok(())
}

/// Reports the rows that one removal deleted.
#[derive(Debug)]
pub struct Removed {
    pub dict_id: i64,
    pub dicts: usize,
    pub sources: usize,
    /// Counts blobs dropped after the sweep. The count can be lower than the
    /// media row count when another Dictionary ships the same asset.
    pub blobs: usize,
    /// Counts rows deleted from each Dictionary-keyed table, in [`DICT_KEYED`] order.
    ///
    /// One array holds the counts and the table order. When the schema gains a
    /// table, both the deletion walk and this report gain that table together.
    /// Read this array through [`Removed::rows_in`] or a named accessor.
    /// Do not read it by index.
    pub rows: [usize; DICT_KEYED.len()],
}

impl Removed {
    /// Returns the count for one table name.
    ///
    /// Returns `0` for a table that this build does not know. A removal that
    /// visits no such table deletes no rows from it.
    pub fn rows_in(&self, table: &str) -> usize {
        DICT_KEYED.iter().position(|t| *t == table).map_or(0, |i| self.rows[i])
    }

    pub fn terms(&self) -> usize {
        self.rows_in("term")
    }

    pub fn entries(&self) -> usize {
        self.rows_in("entry")
    }

    pub fn media(&self) -> usize {
        self.rows_in("media")
    }
}

/// Removes one Dictionary.
///
/// An absent `dict_id` is a no-op.
///
/// Delete rows from each table in [`DICT_KEYED`] in child-first order. Then
/// delete the `dict` row. Check the list against the live schema before any
/// delete with [`refuse_unlisted_tables`]. A schema change can leave child rows
/// that reference the removed `dict`.
pub fn remove_dictionary(conn: &mut Connection, dict_id: i64, archive: &Path) -> Result<Removed> {
    let tx = conn.transaction().context("opening the removal transaction")?;
    crate::dict::build::ensure_indexes(&tx)?;
    refuse_unlisted_tables(&tx)?;

    let mut rows = [0usize; DICT_KEYED.len()];
    for (slot, table) in rows.iter_mut().zip(DICT_KEYED) {
        *slot = delete_rows(&tx, table, dict_id)?;
    }
    let blobs = sweep_orphan_blobs(&tx)?;
    let dicts = delete_rows(&tx, "dict", dict_id)?;
    let sources = if dicts == 0 { 0 } else { forget_source(&tx, archive)? };
    refresh_stats(&tx)?;

    tx.commit().context("committing the removal")?;
    Ok(Removed { dict_id, dicts, sources, blobs, rows })
}

/// Rejects a removal when the schema has an unknown Dictionary-keyed table.
///
/// The schema and [`DICT_KEYED`] list must stay in sync. A schema table such
/// as `dict_style` can appear while the list remains unchanged. An empty
/// `dict_style` table does not expose that mismatch in tests. A later removal
/// can then leave a child row and hit a foreign-key error.
///
/// Check the live schema inside the transaction before any delete. If this
/// build's DDL creates a table with a `dict_id` column and [`DICT_KEYED`] does
/// not list it, reject the removal and name the table.
fn refuse_unlisted_tables(conn: &Connection) -> Result<()> {
    let present = crate::dict::build::dict_keyed_tables(conn)?;
    let unlisted: Vec<&str> = present
        .iter()
        .map(String::as_str)
        // `dict` is the parent row. Delete it last and count it separately.
        .filter(|t| *t != "dict" && !DICT_KEYED.contains(t))
        .collect();
    if !unlisted.is_empty() {
        anyhow::bail!(
            "{} is keyed on dict_id and this removal would leave its rows behind; \
             add it to dict::build::DICT_KEYED, children before parents",
            unlisted.join(", ")
        );
    }
    Ok(())
}

/// Deletes blobs that no `media` row references.
///
/// A content-addressed blob can serve several Dictionaries and paths. A
/// removal deletes only that Dictionary's `media` rows. It cannot delete a
/// blob until no other Dictionary references the asset.
/// The sweep runs during Dictionary removal, not during hover.
///
/// `media_blob` is not in [`DICT_KEYED`] because it has no `dict_id`. Its
/// `hash` identifies each row by content. Run this sweep after the
/// per-Dictionary deletes, when the rows that remain show all live references.
fn sweep_orphan_blobs(conn: &Connection) -> Result<usize> {
    conn.execute("DELETE FROM media_blob WHERE blob_id NOT IN (SELECT blob_id FROM media)", [])
        .context("dropping unreferenced media blobs")
}

/// Updates planner statistics with a bounded `ANALYZE`.
fn refresh_stats(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA analysis_limit = 400; PRAGMA optimize;")
        .context("refreshing the planner statistics")
}

/// Deletes rows for one `dict_id` from one table.
fn delete_rows(conn: &Connection, table: &str, dict_id: i64) -> Result<usize> {
    let sql = format!("DELETE FROM {table} WHERE dict_id = ?1");
    conn.execute(&sql, params![dict_id]).with_context(|| format!("running {sql}"))
}

/// Removes one archive from `meta`.
pub fn forget_source(conn: &Connection, archive: &Path) -> Result<usize> {
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
        let freqs = [fixture("freq.zip")];
        fixture_db_with_freqs(test_name, &freqs)
    }

    fn fixture_db_with_freqs(
        test_name: &str,
        freqs: &[PathBuf],
    ) -> (Connection, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = TempDbGuard(out.clone());
        let terms = [fixture("terms.zip")];
        crate::dict::build::build(&terms, freqs, &out, &|_| {}).unwrap();
        (Connection::open(&out).unwrap(), guard)
    }

    #[test]
    fn reapply_frequencies_updates_all_terms() {
        let (mut conn, _guard) = fixture_db_with_freqs("reapply_updates", &[]);
        let freqs = [fixture("freq.zip")];

        let count = reapply_frequencies(&mut conn, &freqs, &|_| {}).unwrap();

        assert_eq!(5, count);
        let freq: i64 = conn
            .query_row("SELECT freq FROM term WHERE surface = ?1", ["食べる"], |r| r.get(0))
            .unwrap();
        assert_eq!(7, freq);
    }

    #[test]
    fn reapply_frequencies_with_empty_list_nulls_everything() {
        let (mut conn, _guard) = fixture_db("reapply_clears");

        reapply_frequencies(&mut conn, &[], &|_| {}).unwrap();

        let ranked: i64 = conn
            .query_row("SELECT COUNT(*) FROM term WHERE freq IS NOT NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(0, ranked);
    }

    #[test]
    fn reapply_frequencies_on_empty_db_is_a_no_op() {
        let (mut conn, _guard) = fixture_db("reapply_empty");
        conn.execute("DELETE FROM term", []).unwrap();
        let freqs = [fixture("freq.zip")];

        assert_eq!(0, reapply_frequencies(&mut conn, &freqs, &|_| {}).unwrap());
    }

    fn add_dict(conn: &Connection, dict_id: i64) {
        conn.execute(
            "INSERT INTO dict (dict_id, name, priority) VALUES (?1, ?2, 0)",
            rusqlite::params![dict_id, format!("dict {dict_id}")],
        )
        .unwrap();
    }

    /// The synthetic Dictionary that these tests add beside the built
    /// Dictionaries.
    ///
    /// The fixture build assigns ids 1 and 2 to `terms.zip` and `freq.zip`.
    /// This Dictionary uses the next id, because a frequency Dictionary also
    /// has its own `dict_id` and the user can order and enable it.
    const SECOND: i64 = 3;

    fn add_entry(conn: &Connection, entry_id: i64, dict_id: i64) {
        conn.execute(
            "INSERT INTO entry (entry_id, dict_id, glossary) VALUES (?1, ?2, '[]')",
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

    /// `SECOND`: two entries and three `term` rows.
    fn add_second_dictionary(conn: &Connection) {
        add_dict(conn, SECOND);
        add_entry(conn, 4, SECOND);
        add_entry(conn, 5, SECOND);
        add_term(conn, "ねずみ", 4, SECOND);
        add_term(conn, "いぬ", 5, SECOND);
        add_term(conn, "うま", 5, SECOND);
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

    /// Returns every stored value for one `dict_id`.
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
        assert_eq!(3, next_dict_id(&conn).unwrap(), "terms.zip is 1 and freq.zip is 2");
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
        conn.execute_batch(
            "DELETE FROM reported_freq; DELETE FROM term; DELETE FROM entry; DELETE FROM dict;",
        )
        .unwrap();

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
        assert_eq!(vec![1, 2, 7], dicts);

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

    /// Adds `n` entries and `2n` `term` rows for one `dict_id`.
    fn add_bulk_dictionary(conn: &Connection, dict_id: i64, n: i64) {
        add_dict(conn, dict_id);
        let base = dict_id * 1_000_000;
        conn.execute_batch(&format!(
            "INSERT INTO entry (entry_id, dict_id, glossary)
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

        remove_dictionary(&mut conn, SECOND, Path::new("other.zip")).unwrap();

        assert_eq!(before, snapshot(&conn, 1), "the survivor's bytes must not move");
        assert_eq!(3, count(&conn, "SELECT COUNT(*) FROM entry"));
        assert_eq!(5, count(&conn, "SELECT COUNT(*) FROM term"));
        assert_eq!(vec![1, 2], ids(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"));
    }

    #[test]
    fn a_removal_deletes_the_terms_the_entries_and_the_dict_row() {
        let (mut conn, _guard) = fixture_db("removal_deletes_all_three");
        add_second_dictionary(&conn);
        let survivor = snapshot(&conn, SECOND);

        let gone = remove_dictionary(&mut conn, 1, Path::new("terms.zip")).unwrap();

        assert_eq!(1, gone.dict_id);
        assert_eq!(1, gone.dicts);
        assert_eq!(3, gone.entries());
        assert_eq!(5, gone.terms());
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM term WHERE dict_id = 1"));
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM entry WHERE dict_id = 1"));
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM dict WHERE dict_id = 1"));
        assert_eq!(survivor, snapshot(&conn, SECOND), "the synthetic dict must be untouched");
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
        let two = snapshot(&conn, SECOND);
        let sources = source_names(&conn);

        let gone = remove_dictionary(&mut conn, 99, Path::new("terms.zip"))
            .expect("an absent dictionary is not an error");

        assert_eq!(0, gone.dicts);
        assert_eq!(0, gone.entries());
        assert_eq!(0, gone.terms());
        assert_eq!(0, gone.sources);
        assert_eq!(one, snapshot(&conn, 1));
        assert_eq!(two, snapshot(&conn, SECOND));
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

    /// A Pitch Dictionary removal removes its accents through the same
    /// [`DICT_KEYED`] walk as every other Dictionary-keyed table. The other Dictionary
    /// keeps its own accents.
    #[test]
    fn removing_a_pitch_dictionary_removes_its_accents() {
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_pitch_removal.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let _guard = TempDbGuard(out.clone());
        crate::dict::build::build(
            &[fixture("terms.zip"), fixture("pitch.zip"), fixture("pitch2.zip")],
            &[],
            &out,
            &|_| {},
        )
        .unwrap();
        let mut conn = Connection::open(&out).unwrap();

        let first: i64 = conn
            .query_row("SELECT dict_id FROM dict WHERE name = 'FixturePitch'", [], |r| r.get(0))
            .unwrap();
        let before = count(&conn, "SELECT COUNT(*) FROM pitch");
        let mine = count(&conn, &format!("SELECT COUNT(*) FROM pitch WHERE dict_id = {first}"));
        assert!(mine > 0 && before > mine, "two pitch dictionaries: {before} rows, {mine} its");

        let gone = remove_dictionary(&mut conn, first, Path::new("pitch.zip")).unwrap();

        assert_eq!(mine as usize, gone.rows_in("pitch"), "counted through DICT_KEYED");
        assert_eq!(
            0,
            count(&conn, &format!("SELECT COUNT(*) FROM pitch WHERE dict_id = {first}"))
        );
        assert_eq!(
            before - mine,
            count(&conn, "SELECT COUNT(*) FROM pitch"),
            "the other pitch dictionary keeps every accent it gave"
        );
    }

    /// An import stores the archive's Pitch patterns under the new Dictionary.
    /// A Pitch Dictionary added from the settings window works without a full build.
    #[test]
    fn adding_a_pitch_archive_stores_its_accents_under_the_new_dictionary() {
        let (mut conn, _guard) = fixture_db("adding_pitch");

        let added = add_dictionary(&mut conn, &fixture("pitch.zip"), &[], &|_| {}).unwrap();

        assert_eq!(0, added.entries, "a pitch-only archive contributes no entry");
        let mine = count(
            &conn,
            &format!("SELECT COUNT(*) FROM pitch WHERE dict_id = {}", added.dict_id),
        );
        assert!(mine > 0, "its accents went in under its own dictionary row");
    }

    #[test]
    fn a_full_archive_path_matches_the_recorded_file_name() {
        let (mut conn, _guard) = fixture_db("full_path_still_matches");
        // Use forward slashes on purpose. `Path` parses them as separators on Windows
        // and Linux, so this test covers the same contract in the platform-neutral
        // core on both platforms.
        let path = Path::new("C:/Users/Stella/chibipop/library/terms.zip");

        let gone = remove_dictionary(&mut conn, 1, path).unwrap();

        assert_eq!(1, gone.sources, "source_hashes stores the file name, not the path");
        assert_eq!(vec!["freq.zip"], source_names(&conn));
    }

    #[test]
    fn an_unlisted_archive_leaves_source_hashes_alone() {
        let (mut conn, _guard) = fixture_db("unlisted_archive");
        add_second_dictionary(&conn);

        let gone = remove_dictionary(&mut conn, SECOND, Path::new("never-built-from.zip")).unwrap();

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
        add_bulk_dictionary(&conn, SECOND, 3_000);
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
        add_term(&conn, "orphan", 1, SECOND);
        let before = snapshot(&conn, 1);
        let sources = source_names(&conn);

        let err = remove_dictionary(&mut conn, 1, Path::new("terms.zip"))
            .expect_err("a dangling term row must abort the removal");
        assert!(format!("{err:#}").to_lowercase().contains("foreign key"), "got: {err:#}");

        assert_eq!(before, snapshot(&conn, 1), "the term deletes must roll back too");
        assert_eq!(sources, source_names(&conn));
    }

    fn terms_zip() -> PathBuf {
        fixture("terms.zip")
    }

    fn freq_zip() -> Vec<PathBuf> {
        vec![fixture("freq.zip")]
    }

    /// Copies `terms.zip` with a new file name.
    fn copied_archive(test_name: &str, as_name: &str) -> (PathBuf, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join(format!("a_{}_{test_name}_{as_name}", std::process::id()));
        std::fs::copy(terms_zip(), &dest).unwrap();
        let guard = TempDbGuard(dest.clone());
        (dest, guard)
    }

    fn source_record(conn: &Connection, name: &str) -> Value {
        let raw: String = conn
            .query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
            .unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        let found = parsed.as_array().unwrap().iter().find(|v| v["name"] == name).cloned();
        found.unwrap_or_else(|| panic!("no source record named {name} in {raw}"))
    }

    fn of_dict(conn: &Connection, sql: &str, dict_id: i64) -> Vec<String> {
        rows(conn, &sql.replace("{d}", &dict_id.to_string()))
    }

    #[test]
    fn adding_a_dictionary_leaves_every_existing_row_byte_identical() {
        let (mut conn, _guard) = fixture_db("add_leaves_others_intact");
        add_second_dictionary(&conn);
        let one = snapshot(&conn, 1);
        let two = snapshot(&conn, SECOND);

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        assert_eq!(one, snapshot(&conn, 1), "the built dictionary's bytes must not move");
        assert_eq!(two, snapshot(&conn, SECOND), "nor the synthetic one's");
        assert_eq!(3, made.entries);
        assert_eq!(5, made.terms);
        let of_added = |table: &str| {
            count(&conn, &format!("SELECT COUNT(*) FROM {table} WHERE dict_id = {}", made.dict_id))
        };
        assert_eq!(3, of_added("entry"));
        assert_eq!(5, of_added("term"));
    }

    #[test]
    fn an_added_dictionary_lands_above_every_existing_id() {
        let (mut conn, _guard) = fixture_db("add_allocates_above_max");
        populate(&conn);
        assert_eq!(7, count(&conn, "SELECT MAX(dict_id) FROM dict"));
        assert_eq!(41, count(&conn, "SELECT MAX(entry_id) FROM entry"));

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        assert_eq!(8, made.dict_id, "MAX(dict_id) + 1, not the dictionary count");
        assert_eq!(42, made.first_entry_id);
        assert_eq!(vec![42, 43, 44], ids(&conn, "SELECT entry_id FROM entry WHERE dict_id = 8"));
    }

    #[test]
    fn an_addition_never_lands_on_an_existing_entry_id() {
        let (mut conn, _guard) = fixture_db("add_never_reuses_an_entry_id");
        let before = rows(&conn, "SELECT entry_id, dict_id, glossary FROM entry ORDER BY entry_id");
        assert_eq!(3, before.len());

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        let after = rows(&conn, "SELECT entry_id, dict_id, glossary FROM entry WHERE dict_id = 1 \
                                 ORDER BY entry_id");
        assert_eq!(before, after, "no existing entry row may change");
        assert_eq!(6, count(&conn, "SELECT COUNT(*) FROM entry"), "3 kept + 3 inserted");
        assert_eq!(
            6,
            count(&conn, "SELECT COUNT(DISTINCT entry_id) FROM entry"),
            "an overwrite keeps the count constant; distinct ids catch it"
        );
        let sql = "SELECT COUNT(*) FROM entry WHERE dict_id = {d} AND entry_id <= 3";
        assert_eq!(vec!["Integer(0)"], of_dict(&conn, sql, made.dict_id));
    }

    #[test]
    fn the_added_rows_match_a_full_build_of_the_same_archive() {
        let (reference, _rguard) = fixture_db("add_reference_build");
        let (mut conn, _guard) = fixture_db("add_matches_a_full_build");

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        let cols = "SELECT surface, written, reading, pos, freq FROM term";
        assert_eq!(
            rows(&reference, &format!("{cols} ORDER BY rowid")),
            of_dict(&conn, &format!("{cols} WHERE dict_id = {{d}} ORDER BY rowid"), made.dict_id),
            "an incremental add must write exactly what a rebuild writes"
        );
        assert_eq!(
            rows(&reference, "SELECT glossary FROM entry ORDER BY entry_id"),
            of_dict(
                &conn,
                "SELECT glossary FROM entry WHERE dict_id = {d} ORDER BY entry_id",
                made.dict_id
            ),
        );
        assert_eq!("FixtureTerms", made.name, "the name comes from index.json's title");
        let stored: String = conn
            .query_row("SELECT name FROM dict WHERE dict_id = ?1", [made.dict_id], |r| r.get(0))
            .unwrap();
        assert_eq!("FixtureTerms", stored, "and is what lands in dict.name");
    }

    #[test]
    fn only_a_frequency_archive_ranks_the_added_dictionary() {
        let (mut ranked, _g1) = fixture_db("add_with_freq");
        let with = add_dictionary(&mut ranked, &terms_zip(), &freq_zip(), &|_| {}).unwrap();
        let sql = "SELECT freq FROM term WHERE dict_id = {d} AND surface = '食べる'";
        assert_eq!(vec!["Integer(7)"], of_dict(&ranked, sql, with.dict_id), "freq.zip ranks it");

        let (mut bare, _g2) = fixture_db("add_without_freq");
        let without = add_dictionary(&mut bare, &terms_zip(), &[], &|_| {}).unwrap();
        assert_eq!(vec!["Null"], of_dict(&bare, sql, without.dict_id), "no archive, no rank");
    }

    #[test]
    fn an_added_dictionary_keeps_the_builders_priority_relation() {
        let (mut conn, _guard) = fixture_db("add_priority_relation");
        populate(&conn);

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        assert_eq!(8, made.dict_id);
        let sql = "SELECT priority FROM dict WHERE dict_id = {d}";
        assert_eq!(vec!["Integer(7)"], of_dict(&conn, sql, made.dict_id), "one below dict_id");
        assert_eq!(vec!["Integer(0)"], of_dict(&conn, sql, 1), "as build-dict itself writes");
    }

    #[test]
    fn the_added_archive_is_recorded_in_source_hashes() {
        let (mut conn, _guard) = fixture_db("add_appends_a_source");
        let (extra, _eguard) = copied_archive("add_appends_a_source", "extra.zip");
        let name = extra.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(vec!["terms.zip", "freq.zip"], source_names(&conn));

        add_dictionary(&mut conn, &extra, &[], &|_| {}).unwrap();

        assert_eq!(vec!["terms.zip".to_string(), "freq.zip".to_string(), name.clone()],
                   source_names(&conn), "the new archive is appended, the old ones stay");
        let rec = source_record(&conn, &name);
        assert_eq!(
            Some("b1a8876d676bcea6accb3e1f0c1c20b539cebad7652723108b0b2538ab4056a6"),
            rec["sha256"].as_str(),
            "the same hash build-dict records for terms.zip"
        );
        assert_eq!(Some(std::fs::metadata(&extra).unwrap().len()), rec["bytes"].as_u64());
    }

    #[test]
    fn re_adding_a_listed_archive_does_not_duplicate_its_source_record() {
        let (mut conn, _guard) = fixture_db("add_replaces_a_source");

        add_dictionary(&mut conn, &terms_zip(), &[], &|_| {}).unwrap();

        assert_eq!(
            vec!["freq.zip", "terms.zip"],
            source_names(&conn),
            "one record per archive name, however often it is added"
        );
    }

    #[test]
    fn adding_to_an_emptied_database_starts_at_the_builders_base() {
        let (mut conn, _guard) = fixture_db("add_to_an_empty_database");
        conn.execute_batch(
            "DELETE FROM reported_freq; DELETE FROM term; DELETE FROM entry; DELETE FROM dict;",
        )
        .unwrap();

        let made = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        assert_eq!(1, made.dict_id, "the empty-table base is 1, as the builder writes");
        assert_eq!(1, made.first_entry_id);
        assert_eq!(vec![1, 2, 3], ids(&conn, "SELECT entry_id FROM entry ORDER BY entry_id"));
        assert_eq!(0, count(&conn, "SELECT priority FROM dict WHERE dict_id = 1"));
    }

    /// A Dictionary can supply frequency data without a term bank. The import
    /// adds no `term` rows, but it still gets a `dict` row. Stored Reported
    /// frequencies and Pitch patterns can use that id. The previous implementation
    /// rejected an archive when its name held `Freq`.
    #[test]
    fn a_frequency_archive_is_added_as_a_dictionary_that_contributes_no_entries() {
        let (mut conn, _guard) = fixture_db("add_a_freq_archive");

        let made = add_dictionary(&mut conn, &fixture("freq.zip"), &[], &|_| {})
            .expect("an archive supplying frequency is still a Dictionary");

        assert_eq!("FixtureFreq", made.name);
        assert_eq!(0, made.entries, "no term bank, so no entry");
        assert_eq!(0, made.terms);
        assert_eq!(
            vec![1, 2, made.dict_id],
            ids(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"),
        );
        assert!(
            source_names(&conn).contains(&"freq.zip".to_string()),
            "and the build records what it read: {:?}",
            source_names(&conn),
        );
    }

    #[test]
    fn a_failure_part_way_through_rolls_the_whole_addition_back() {
        let (mut conn, _guard) = fixture_db("add_rolls_back");
        let before = snapshot(&conn, 1);
        let sources = source_names(&conn);
        conn.execute_batch(
            "CREATE TEMP TRIGGER burst BEFORE INSERT ON term WHEN NEW.surface = 'ねこ'
             BEGIN SELECT RAISE(ABORT, 'burst'); END;",
        )
        .unwrap();

        let err = add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {})
            .expect_err("an aborted insert must fail the addition");

        assert!(format!("{err:#}").contains("burst"), "got: {err:#}");
        assert_eq!(vec![1, 2], ids(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"),
                   "the dict row goes in first and must roll back with the rest");
        assert_eq!(3, count(&conn, "SELECT COUNT(*) FROM entry"));
        assert_eq!(5, count(&conn, "SELECT COUNT(*) FROM term"));
        assert_eq!(before, snapshot(&conn, 1));
        assert_eq!(sources, source_names(&conn), "a rolled-back add records no source");
    }

    #[test]
    fn an_addition_refreshes_grossly_stale_planner_statistics() {
        let (mut conn, _guard) = fixture_db("add_refreshes_stats");
        add_bulk_dictionary(&conn, SECOND, 3_000);
        assert_eq!("5 2", term_stat(&conn), "the build's ANALYZE saw 5 term rows");

        add_dictionary(&mut conn, &terms_zip(), &freq_zip(), &|_| {}).unwrap();

        let after = term_stat(&conn);
        let seen: i64 = after.split(' ').next().unwrap().parse().unwrap();
        assert_eq!(6_010, count(&conn, "SELECT COUNT(*) FROM term"), "5 built + 6,000 + 5 added");
        assert!(seen > 1_000, "an edit must not leave the planner reading {after}");
    }

    // ---- Media store ----

    fn media_zip() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/media.zip")
    }

    /// A content-addressed blob is shared across Dictionaries and paths. A removal
    /// cannot delete its blobs until no other Dictionary references them.
    #[test]
    fn removing_a_dictionary_drops_its_media_and_only_the_orphaned_blobs() {
        let (mut conn, _guard) = fixture_db("remove_media");
        // Add the same archive twice. Each blob then has two Dictionary references.
        let first = add_dictionary(&mut conn, &media_zip(), &[], &|_| {}).unwrap();
        let second = add_dictionary(&mut conn, &media_zip(), &[], &|_| {}).unwrap();
        assert_eq!(16, count(&conn, "SELECT COUNT(*) FROM media"), "eight paths, twice");
        assert_eq!(7, count(&conn, "SELECT COUNT(*) FROM media_blob"), "and one set of blobs");

        let gone = remove_dictionary(&mut conn, first.dict_id, &media_zip()).unwrap();
        assert_eq!(8, gone.media());
        assert_eq!(0, gone.blobs, "the surviving dictionary still ships every asset");
        assert_eq!(8, count(&conn, "SELECT COUNT(*) FROM media"));
        assert_eq!(7, count(&conn, "SELECT COUNT(*) FROM media_blob"));

        let gone = remove_dictionary(&mut conn, second.dict_id, &media_zip()).unwrap();
        assert_eq!(8, gone.media());
        assert_eq!(7, gone.blobs, "the last reference going takes the bytes with it");
        assert_eq!(0, count(&conn, "SELECT COUNT(*) FROM media_blob"));
    }

    /// A previous build can fill the live database's blob table. An addition must
    /// reuse each row with the same content instead of a duplicate.
    #[test]
    fn adding_a_dictionary_reuses_the_blobs_a_previous_build_wrote() {
        let (mut conn, _guard) = fixture_db("add_media_reuses_blobs");
        add_dictionary(&mut conn, &media_zip(), &[], &|_| {}).unwrap();
        let blobs = ids(&conn, "SELECT blob_id FROM media WHERE path = 'gaiji/one.png'");

        add_dictionary(&mut conn, &media_zip(), &[], &|_| {}).unwrap();

        let after = ids(&conn, "SELECT blob_id FROM media WHERE path = 'gaiji/one.png'");
        assert_eq!(2, after.len(), "two dictionaries, two rows");
        assert_eq!(vec![blobs[0], blobs[0]], after, "and one blob behind both");
        assert_eq!(7, count(&conn, "SELECT COUNT(*) FROM media_blob"));
    }

    // ---- remove one Dictionary and add it again ----

    /// One archive with each row type that uses a `dict_id`: a term bank, the
    /// Dictionary's `styles.css`, and a referenced image asset.
    ///
    /// Create this archive in the test because it must be *complete*.
    /// `tests/fixtures/yomitan/terms.zip` has no stylesheet or media, so tests
    /// based on it leave `dict_style` and `media` empty. That empty state let
    /// an earlier removal omit `dict_style` without a failure. The PNG comes
    /// from `tests/fixtures/media`, so this test adds no binary file.
    fn complete_archive(test_name: &str) -> (PathBuf, TempDbGuard) {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("a_{}_{test_name}_complete.zip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let png = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/one.png");
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("index.json", opts).unwrap();
        zip.write_all(br#"{"title":"FixtureComplete","format":3,"revision":"1"}"#).unwrap();
        zip.start_file("term_bank_1.json", opts).unwrap();
        zip.write_all(
            br#"[["\u732b","\u306d\u3053","","",0,
                 [{"type":"structured-content","content":[
                   {"tag":"img","path":"gaiji/one.png","height":1.0,"sizeUnits":"em"},
                   {"tag":"span","data":{"fbox":"1"},"content":"cat"}]}],0,""]]"#,
        )
        .unwrap();
        zip.start_file("styles.css", opts).unwrap();
        zip.write_all(b"span[data-sc-fbox] { padding: 0.1em }\n").unwrap();
        zip.start_file("gaiji/one.png", opts).unwrap();
        zip.write_all(&std::fs::read(&png).unwrap()).unwrap();
        zip.finish().unwrap();
        (path.clone(), TempDbGuard(path))
    }

    /// Opens a database with exactly one Dictionary. The real builder creates it
    /// from `archive`.
    fn db_from(test_name: &str, archive: &Path) -> (Connection, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_edit_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = TempDbGuard(out.clone());
        let terms = [archive.to_path_buf()];
        crate::dict::build::build(&terms, &[], &out, &|_| {}).unwrap();
        (Connection::open(&out).unwrap(), guard)
    }

    fn integrity(conn: &Connection) -> String {
        conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap()
    }

    fn fk_violations(conn: &Connection) -> Vec<String> {
        rows(conn, "PRAGMA foreign_key_check")
    }

    /// Test path for a user: remove one Dictionary, then add the same archive
    /// again.
    #[test]
    fn removing_a_dictionary_and_adding_it_back_leaves_a_sound_database() {
        let (archive, _aguard) = complete_archive("round_trip");
        let (mut conn, _guard) = db_from("round_trip", &archive);
        assert_eq!(1, count(&conn, "SELECT COUNT(*) FROM dict_style"), "the fixture ships one");
        assert_eq!(1, count(&conn, "SELECT COUNT(*) FROM media"), "and one asset");

        remove_dictionary(&mut conn, 1, &archive).expect("the removal must succeed");
        let made = add_dictionary(&mut conn, &archive, &[], &|_| {})
            .expect("adding the same archive back must succeed");

        assert_eq!("ok", integrity(&conn), "the database must still be sound");
        assert!(fk_violations(&conn).is_empty(), "no dangling reference may survive");
        assert_eq!(
            1,
            count(&conn, "SELECT COUNT(*) FROM dict_style"),
            "one dictionary, one stylesheet"
        );
        // Check the term that the removal made unavailable to the popup.
        let sql = "SELECT COUNT(*) FROM term WHERE surface = 'ねこ' AND dict_id = {d}";
        assert_eq!(vec!["Integer(1)"], of_dict(&conn, sql, made.dict_id), "the term is back");
    }

    /// A removal leaves no row with the removed `dict_id` in **any** table. The
    /// table set comes from the schema, not a list written in this test.
    ///
    /// This test avoids a second list of tables. When the DDL adds a
    /// `dict_id` column, this test detects a removal that does not delete that
    /// table. The complete archive puts rows in every table that the check
    /// visits. An empty table cannot prove that the removal handled it.
    #[test]
    fn a_removal_leaves_no_row_keyed_on_the_removed_dictionary_in_any_table() {
        let (archive, _aguard) = complete_archive("no_rows_left");
        let (mut conn, _guard) = db_from("no_rows_left", &archive);
        // The fixture archive has a term bank but no frequency or pitch bank.
        // Add one claim and one accent under its `dict_id` so every Dictionary-keyed
        // table has a row. An empty table cannot prove that removal handles it.
        conn.execute(
            "INSERT INTO reported_freq (dict_id, term, reading, rank) VALUES (1, 'ねこ', NULL, 3)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pitch (dict_id, term, reading, downstep, pattern, nasal, devoice, tags) \
             VALUES (1, 'ねこ', 'ねこ', 1, NULL, '[]', '[]', '[]')",
            [],
        )
        .unwrap();
        let keyed = crate::dict::build::dict_keyed_tables(&conn).unwrap();
        assert!(keyed.len() >= 6, "the schema's dict-keyed tables: {keyed:?}");
        for table in &keyed {
            let held = count(&conn, &format!("SELECT COUNT(*) FROM {table} WHERE dict_id = 1"));
            assert!(held > 0, "{table} has no row for dict 1, so checking it proves nothing");
        }

        remove_dictionary(&mut conn, 1, &archive).expect("the removal must succeed");

        for table in &keyed {
            assert_eq!(
                0,
                count(&conn, &format!("SELECT COUNT(*) FROM {table} WHERE dict_id = 1")),
                "{table} still holds rows for the removed dictionary",
            );
        }
        assert!(fk_violations(&conn).is_empty(), "and nothing dangles");
    }

    /// The guard rejects a schema table with a `dict_id` but no
    /// [`DICT_KEYED`] entry. It names the table before a row moves.
    ///
    /// This test covers the production check. A new table gets a direct message,
    /// not a later foreign-key error. A database with an unknown table stays
    /// unchanged.
    #[test]
    fn a_dict_keyed_table_the_removal_does_not_know_aborts_it_by_name() {
        let (mut conn, _guard) = fixture_db("unlisted_table_refused");
        conn.execute_batch(
            "CREATE TABLE dict_note (
                 dict_id INTEGER PRIMARY KEY REFERENCES dict(dict_id),
                 note    TEXT NOT NULL
             );
             INSERT INTO dict_note (dict_id, note) VALUES (1, 'a note the removal does not know');",
        )
        .unwrap();
        let before = snapshot(&conn, 1);

        let err = remove_dictionary(&mut conn, 1, Path::new("terms.zip"))
            .expect_err("an unlisted dict-keyed table must abort the removal");

        let text = format!("{err:#}");
        assert!(text.contains("dict_note"), "the message has to name the table: {text}");
        assert!(text.contains("DICT_KEYED"), "and say what to do about it: {text}");
        assert_eq!(before, snapshot(&conn, 1), "and nothing may have moved");
        assert_eq!(1, count(&conn, "SELECT COUNT(*) FROM dict_note"));
    }
}
