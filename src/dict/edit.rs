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

    fn of_dict(conn: &Connection, sql: &str, dict_id: i64) -> Vec<String> {
        rows(conn, &sql.replace("{d}", &dict_id.to_string()))
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

    // ---- Media store ----

    // ---- remove one Dictionary and add it again ----

}
