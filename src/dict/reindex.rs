//! Recomputes Frequency ranks from stored Reported frequencies.
//!
//! A Reindex updates local rows in one SQL transaction. It never reads an
//! archive, creates a second database file, or renames a file. The full
//! archive build does that work through
//! [`crate::dict::build::build`] (ARCHITECTURE.md#settings-and-config).
//!
//! The promoted database has `PRAGMA journal_mode = WAL`. A reader can use the
//! old rank while the transaction is open, then use the new rank after commit.
//! The daemon receives the new rank after the `reload` verb reaches the control
//! socket.

use crate::dict::frequency::{lookup_freq, reduce, FreqSource, FreqTable, RankingStrategy};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::BTreeSet;

/// The `meta` key for enabled frequency Dictionaries, in order.
const ORDER_KEY: &str = "frequency_order";

/// The `meta` key for the Ranking strategy that produced Frequency ranks.
const STRATEGY_KEY: &str = "frequency_strategy";

/// Inputs that produced this database's Frequency ranks.
///
/// The database records these inputs in `meta`. It does not derive them from
/// config because `term.freq` is derived state. The popup must use the inputs
/// that produced the stored ranks. A reader that uses config order could show a
/// Reported frequency from a Dictionary that the stored Frequency ranks did not use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reduction {
    /// `dict_id` values for enabled frequency Dictionaries, highest priority first.
    /// A Dictionary absent from this list is disabled. A disabled Dictionary
    /// contributes no rank, but its claims stay stored. One Reindex enables it
    /// again. Do not import it again.
    pub order: Vec<i64>,
    pub strategy: RankingStrategy,
}

/// Recomputes every Frequency rank from stored Reported frequencies in one
/// transaction.
///
/// `enabled` lists the frequency Dictionaries that the user enabled, in
/// frequency-list order. Position sets priority inside the frequency role.
/// The pass ignores a name that does not match an installed Dictionary. A
/// config can retain a removed Dictionary name without an error.
///
/// `strategy` reduces claims from Dictionaries that have a headword to the
/// `term.freq` rank. When none has the headword, the pass stores `NULL`.
/// `score` then keeps its `DEFAULT_FREQ` value.
///
/// Returns the number of `term` rows that the pass restamped. If the pass
/// fails, the transaction leaves every rank unchanged. This code writes no
/// data outside the transaction.
pub fn reindex(
    conn: &mut Connection,
    enabled: &[String],
    strategy: RankingStrategy,
    on_progress: &dyn Fn(&str),
) -> Result<u64> {
    let tx = conn.transaction().context("opening the reindex transaction")?;
    let reduction = Reduction { order: ids_for(&tx, enabled)?, strategy };
    let restamped = restamp_from_stored(&tx, &reduction, on_progress)?;
    record(&tx, &reduction)?;
    // Do not run `ANALYZE` here. A Reindex changes values in one unindexed
    // column and moves no row. The planner samples no changed data.
    tx.commit().context("committing the reindex")?;
    Ok(restamped)
}

/// Returns the Reduction that produced this database's Frequency ranks.
///
/// If both keys are absent, return an empty order and the default strategy.
/// These values are the stored truth. Do not substitute config values. Every
/// build writes both keys.
/// The two absent keys mean no enabled frequency Dictionary and no ranks.
///
/// Return an error when this function cannot read a value. A hand-edited or
/// corrupt record supplies invalid input. Do not rank from another input
/// without a user message.
pub fn recorded(conn: &Connection) -> Result<Reduction> {
    let order = match meta(conn, ORDER_KEY)? {
        None => Vec::new(),
        Some(raw) => serde_json::from_str(&raw)
            .with_context(|| format!("parsing meta.{ORDER_KEY} ({raw})"))?,
    };
    let strategy = match meta(conn, STRATEGY_KEY)? {
        None => RankingStrategy::default(),
        Some(raw) => raw.parse().with_context(|| format!("reading meta.{STRATEGY_KEY}"))?,
    };
    Ok(Reduction { order, strategy })
}

/// Records the Reduction that stamped the current `term` rows.
pub(crate) fn record(conn: &Connection, reduction: &Reduction) -> Result<()> {
    let order = serde_json::to_string(&reduction.order)
        .with_context(|| format!("writing meta.{ORDER_KEY}"))?;
    for (key, value) in [(ORDER_KEY, order.as_str()), (STRATEGY_KEY, reduction.strategy.as_str())]
    {
        conn.execute("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)", params![key, value])
            .with_context(|| format!("updating meta.{key}"))?;
    }
    Ok(())
}

/// Stores one Dictionary's Reported frequencies.
///
/// The keys of a `FreqTable` become a term and an optional reading.
/// [`lookup_freq`] checks the reading-specific key first, then the
/// reading-agnostic key. These rows therefore produce the same values as the
/// source `FreqTable`.
pub(crate) fn store_reported(
    tx: &Transaction,
    dict_id: i64,
    table: &FreqTable,
) -> Result<usize> {
    let mut insert = tx
        .prepare("INSERT INTO reported_freq (dict_id, term, reading, rank) VALUES (?1, ?2, ?3, ?4)")
        .context("preparing the reported-frequency insert")?;
    for ((term, reading), rank) in table {
        insert
            .execute(params![dict_id, term, reading, rank])
            .with_context(|| format!("storing the reported frequency of {term}"))?;
    }
    Ok(table.len())
}

/// Synchronizes stored Reported frequencies with the active frequency archives
/// and records the resulting Reduction.
///
/// This is the archive path for an import or removal. It does not serve a
/// settings change. `sources` lists every active frequency archive. A named
/// Dictionary gets a `dict` row and its claims. A name that leaves the list
/// loses its claims.
///
/// Keep the recorded strategy because only archives changed. Append a new
/// Dictionary at the end of the order.
pub(crate) fn sync_reported(tx: &Transaction, sources: &[FreqSource]) -> Result<Reduction> {
    let strategy = recorded(tx)?.strategy;
    let mut held = frequency_dictionaries(tx)?;
    let mut order = Vec::with_capacity(sources.len());

    for source in sources {
        let dict_id = match held.iter().position(|(_, name)| *name == source.name) {
            Some(found) => held.remove(found).0,
            // A combined archive already has a `dict` row from its term definitions.
            // Store its frequency claims under that row. Do not create a second row
            // with the same name. Create a row only when the database has no match.
            None => match unclaimed_dict_row(tx, &source.name, &order)? {
                Some(existing) => existing,
                None => new_dict_row(tx, &source.name)?,
            },
        };
        drop_reported(tx, dict_id)?;
        store_reported(tx, dict_id, &source.table)?;
        order.push(dict_id);
    }

    // A Dictionary left in `held` belongs to an archive no longer in the list.
    // A Dictionary that also supplies terms keeps its row and entries, but loses
    // only its claims. A Dictionary with no other data loses its row.
    for (dict_id, _) in held {
        drop_reported(tx, dict_id)?;
        let entries: i64 = tx
            .query_row("SELECT COUNT(*) FROM entry WHERE dict_id = ?1", params![dict_id], |r| {
                r.get(0)
            })
            .with_context(|| format!("counting the entries of dictionary {dict_id}"))?;
        if entries == 0 {
            tx.execute("DELETE FROM dict WHERE dict_id = ?1", params![dict_id])
                .with_context(|| format!("dropping the dict row of dictionary {dict_id}"))?;
        }
    }

    let reduction = Reduction { order, strategy };
    record(tx, &reduction)?;
    Ok(reduction)
}

/// Restamps every Frequency rank from the inputs named by a Reduction. Settings
/// changes and archive changes share this pass.
pub(crate) fn restamp_from_stored(
    tx: &Transaction,
    reduction: &Reduction,
    on_progress: &dyn Fn(&str),
) -> Result<u64> {
    let ranks = reduce(&stored_tables(tx, &reduction.order)?, reduction.strategy);
    restamp(tx, &ranks, on_progress)
}

/// Restamps `term.freq` from one reduced table and returns the count of rows
/// that it writes.
///
/// Write every row, not only rows whose rank changes. The strategy, order, and
/// enabled set are inputs. An unchanged rank confirms that the pass handled
/// the row. Do not skip it.
fn restamp(
    tx: &Transaction,
    ranks: &FreqTable,
    on_progress: &dyn Fn(&str),
) -> Result<u64> {
    let rows: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut query = tx
            .prepare("SELECT rowid, surface, written, reading FROM term")
            .context("preparing the term frequency query")?;
        let mapped = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .context("querying term frequencies")?;
        mapped.collect::<rusqlite::Result<_>>().context("reading term frequencies")?
    };
    let mut update = tx
        .prepare("UPDATE term SET freq = ?1 WHERE rowid = ?2")
        .context("preparing the frequency update")?;
    let mut processed = 0_u64;

    for (rowid, surface, written, reading) in rows {
        // A frequency archive uses the written form when present. For a kana-only
        // headword, it uses the reading.
        let term = written.as_deref().unwrap_or(&surface);
        let rank = lookup_freq(ranks, term, reading.as_deref());
        update
            .execute(params![rank, rowid])
            .with_context(|| format!("updating term row {rowid}"))?;
        processed = processed.checked_add(1).context("frequency update count overflowed")?;
        if processed.is_multiple_of(1000) {
            on_progress(&format!("Updated {processed} frequency rows…"));
        }
    }
    Ok(processed)
}

/// Returns each Dictionary's stored claims in the given order.
///
/// Scan `reported_freq` once instead of querying each Dictionary. Skip disabled
/// claims as the scan reads rows. Frequency Dictionaries are few, and an index
/// does not improve a full scan of one Dictionary.
fn stored_tables(conn: &Connection, order: &[i64]) -> Result<Vec<FreqTable>> {
    let mut tables = vec![FreqTable::new(); order.len()];
    if order.is_empty() {
        return Ok(tables);
    }
    let mut stmt = conn
        .prepare("SELECT dict_id, term, reading, rank FROM reported_freq")
        .context("preparing the reported-frequency query")?;
    let mut rows = stmt.query([]).context("reading the reported frequencies")?;
    while let Some(row) = rows.next().context("reading a reported frequency")? {
        let dict_id: i64 = row.get(0)?;
        let Some(slot) = order.iter().position(|id| *id == dict_id) else { continue };
        tables[slot].insert((row.get(1)?, row.get(2)?), row.get(3)?);
    }
    Ok(tables)
}

/// Returns `dict` rows that the frequency list names, in list order.
///
/// Ignore a name with no installed Dictionary. A config can retain a removed
/// name, and an absent Dictionary reports nothing. If two Dictionaries share
/// a name, include both in `dict_id` order. `dict.name` is a title, so two
/// editions can share it.
fn ids_for(conn: &Connection, names: &[String]) -> Result<Vec<i64>> {
    let mut stmt = conn
        .prepare("SELECT dict_id FROM dict WHERE name = ?1 ORDER BY dict_id")
        .context("preparing the dictionary name query")?;
    let mut order: Vec<i64> = Vec::with_capacity(names.len());
    for name in names {
        let found = stmt
            .query_map([name], |r| r.get::<_, i64>(0))
            .with_context(|| format!("resolving the dictionary named {name}"))?;
        for id in found {
            let id = id.with_context(|| format!("reading the dictionary named {name}"))?;
            if !order.contains(&id) {
                order.push(id);
            }
        }
    }
    Ok(order)
}

/// Returns every Dictionary with stored Reported frequencies, by name.
///
/// Use the union of `reported_freq` and the recorded order. Either source alone
/// misses a case. An archive with no `"freq"` row stores no claim. A disabled
/// Dictionary stays out of the order but keeps its claims.
fn frequency_dictionaries(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut ids: BTreeSet<i64> = recorded(conn)?.order.into_iter().collect();
    {
        let mut stmt = conn
            .prepare("SELECT DISTINCT dict_id FROM reported_freq")
            .context("preparing the reported-frequency dictionary query")?;
        let found = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .context("listing the dictionaries with reported frequencies")?;
        for id in found {
            ids.insert(id.context("reading a dictionary with reported frequencies")?);
        }
    }
    let mut stmt = conn
        .prepare("SELECT name FROM dict WHERE dict_id = ?1")
        .context("preparing the dictionary name lookup")?;
    let mut named = Vec::with_capacity(ids.len());
    for id in ids {
        let name: Option<String> = stmt
            .query_row([id], |r| r.get(0))
            .optional()
            .with_context(|| format!("reading the name of dictionary {id}"))?;
        if let Some(name) = name {
            named.push((id, name));
        }
    }
    Ok(named)
}

/// Returns an unused `dict` row for this name.
///
/// The pass that reads definitions inserts a Dictionary with terms and
/// frequency data once. Its claims use that row. `taken` lists ids already
/// chosen for earlier sources, so two editions with one title get separate rows.
fn unclaimed_dict_row(tx: &Transaction, name: &str, taken: &[i64]) -> Result<Option<i64>> {
    let mut stmt = tx
        .prepare("SELECT dict_id FROM dict WHERE name = ?1 ORDER BY dict_id")
        .context("preparing the dictionary name query")?;
    let found = stmt
        .query_map([name], |r| r.get::<_, i64>(0))
        .with_context(|| format!("resolving the dictionary named {name}"))?;
    for id in found {
        let id = id.with_context(|| format!("reading the dictionary named {name}"))?;
        if !taken.contains(&id) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Creates a `dict` row for a new Dictionary.
///
/// A frequency Dictionary is a Dictionary. The user orders and enables it, and
/// its claims use its own `dict_id`. Removal deletes those claims through the
/// same [`crate::dict::build::DICT_KEYED`] walk as other Dictionary-keyed tables.
/// Keep the builder's `priority` relation, one below `dict_id`. Frequency
/// priority uses the recorded order, not this column.
fn new_dict_row(tx: &Transaction, name: &str) -> Result<i64> {
    let dict_id = crate::dict::edit::next_dict_id(tx)?;
    tx.execute(
        "INSERT INTO dict (dict_id, name, priority) VALUES (?1, ?2, ?3)",
        params![dict_id, name, dict_id - 1],
    )
    .with_context(|| format!("inserting the dict row for {name}"))?;
    Ok(dict_id)
}

fn drop_reported(tx: &Transaction, dict_id: i64) -> Result<usize> {
    tx.execute("DELETE FROM reported_freq WHERE dict_id = ?1", params![dict_id])
        .with_context(|| format!("dropping the reported frequencies of dictionary {dict_id}"))
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT v FROM meta WHERE k = ?1", params![key], |r| r.get(0))
        .optional()
        .with_context(|| format!("reading meta.{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::edit::remove_dictionary;
    use crate::lookup::deconj::Deconjugator;
    use crate::lookup::engine::LookupEngine;
    use crate::lookup::model::{Dictionary, Hit};
    use crate::lookup::sqlite::SqliteDictionary;
    use crate::present::{self, Card};
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    /// The title of `freq.zip` and the title of the second archive.
    const FREQ_A: &str = "FixtureFreq";
    const FREQ_B: &str = "FixtureFreqB";

    struct TempFileGuard(PathBuf);

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    fn scratch(test: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_reindex_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("t_{}_{test}_{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A second frequency archive gives tests two Dictionaries.
    ///
    /// Keep this archive in the test because its values define the fixture. It
    /// differs from `freq.zip` for both shared headwords. Its 猫 rank is lower,
    /// its 食べる rank is higher, and it adds a headword that `freq.zip` lacks.
    fn second_freq_archive(test: &str) -> (PathBuf, TempFileGuard) {
        let path = scratch(test, "freq_b.zip");
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("index.json", opts).unwrap();
        zip.write_all(
            br#"{"title":"FixtureFreqB","format":3,"frequencyMode":"rank-based"}"#,
        )
        .unwrap();
        zip.start_file("term_meta_bank_1.json", opts).unwrap();
        zip.write_all(
            r#"[["食べる","freq",{"value":500}],
                ["猫","freq",{"reading":"ねこ","frequency":{"value":3}}],
                ["ねこ","freq",{"value":12}]]"#
                .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        (path.clone(), TempFileGuard(path))
    }

    /// Builds a real database so Reindex uses rows from the builder.
    fn built(test: &str, freqs: &[PathBuf]) -> (PathBuf, TempFileGuard) {
        let out = scratch(test, "chibipop.sqlite");
        let guard = TempFileGuard(out.clone());
        crate::dict::build::build(&[fixture("terms.zip")], freqs, &out, &|_| {}).unwrap();
        (out, guard)
    }

    /// The Frequency rank for each of the fixture's three headwords.
    ///
    /// Each field uses a surface with one `term` row. `食べる` and `猫` use
    /// written forms. The kana-only `ねこ` entry has no written form.
    #[derive(Debug, PartialEq, Eq)]
    struct Ranks {
        taberu: Option<i64>,
        neko: Option<i64>,
        cat: Option<i64>,
    }

    fn ranks(conn: &Connection) -> Ranks {
        let one = |sql: &str| -> Option<i64> { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        Ranks {
            taberu: one("SELECT freq FROM term WHERE surface = '食べる'"),
            neko: one("SELECT freq FROM term WHERE surface = 'ねこ' AND written IS NULL"),
            cat: one("SELECT freq FROM term WHERE surface = '猫'"),
        }
    }

    /// Stored claim for one headword from each Dictionary, by `dict_id`.
    fn claims_for(conn: &Connection, term: &str, reading: Option<&str>) -> Vec<(i64, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT dict_id, rank FROM reported_freq \
                 WHERE term = ?1 AND reading IS ?2 ORDER BY dict_id",
            )
            .unwrap();
        let found = stmt.query_map(params![term, reading], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        found.map(Result::unwrap).collect()
    }

    fn claims_of(conn: &Connection, dict_id: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM reported_freq WHERE dict_id = ?1",
            params![dict_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Looks up text in a built database through the real engine.
    ///
    /// The inputs are plain forms. No deconjugation rules apply, and the
    /// Deconjugator seeds each form directly.
    fn hits(db: &Path, text: &str) -> Vec<Hit> {
        let dict = SqliteDictionary::open(db).expect("the built database opens");
        LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap()
    }

    /// Builds the popup Card for one headword with `present::build`.
    fn card_for(db: &Path, text: &str, written: Option<&str>) -> Card {
        let dict = SqliteDictionary::open(db).expect("the built database opens");
        let found = LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap();
        let installed = dict.dicts().unwrap();
        // An empty config enables every Dictionary that the database finds. This is the
        // state after a new install.
        let shown = present::build(
            &found,
            &installed,
            &crate::config::Config::default().present_config(&installed),
            &dict,
        );
        shown
            .all_cards
            .into_iter()
            .find(|c| c.written.as_deref() == written)
            .unwrap_or_else(|| panic!("no card for {written:?} in a lookup of {text}"))
    }

    fn both_dictionaries() -> [String; 2] {
        [FREQ_A.to_string(), FREQ_B.to_string()]
    }

    #[test]
    fn two_frequency_dictionaries_each_keep_their_own_number() {
        let (second, _sguard) = second_freq_archive("both_stored");
        let (db, _guard) = built("both_stored", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();

        assert_eq!(
            vec![(2, 42), (3, 3)],
            claims_for(&conn, "猫", Some("ねこ")),
            "one word, two dictionaries, two numbers, each under its own id"
        );

        remove_dictionary(&mut conn, 2, &fixture("freq.zip")).unwrap();

        assert_eq!(vec![(3, 3)], claims_for(&conn, "猫", Some("ねこ")));
        assert_eq!(0, claims_of(&conn, 2), "the removed dictionary keeps nothing");
        assert_eq!(3, claims_of(&conn, 3), "and the survivor loses nothing");
    }

    #[test]
    fn a_build_records_the_reduction_its_ranks_came_from() {
        let (second, _sguard) = second_freq_archive("recorded");
        let (db, _guard) = built("recorded", &[fixture("freq.zip"), second]);
        let conn = Connection::open(&db).unwrap();

        assert_eq!(
            Reduction { order: vec![2, 3], strategy: RankingStrategy::BestRank },
            recorded(&conn).unwrap(),
            "every frequency dictionary, in library order, under the default rule"
        );
    }

    #[test]
    fn each_strategy_stamps_its_own_rank_into_the_term_column() {
        let (second, _sguard) = second_freq_archive("per_strategy");
        let (db, _guard) = built("per_strategy", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();

        // `freq.zip` reports 7 for 食べる and 42 for 猫 with reading ねこ. It has no
        // claim for the kana-only ねこ term. The second archive reports 500, 3, and 12.
        // BestRank picks lower values, Priority picks `freq.zip`, and Median averages
        // each pair. The kana-only ねこ term has one claim, so every strategy returns 12.
        for (strategy, expected) in [
            (RankingStrategy::BestRank, Ranks { taberu: Some(7), neko: Some(12), cat: Some(3) }),
            (RankingStrategy::Priority, Ranks { taberu: Some(7), neko: Some(12), cat: Some(42) }),
            (RankingStrategy::Median, Ranks { taberu: Some(253), neko: Some(12), cat: Some(22) }),
        ] {
            reindex(&mut conn, &both_dictionaries(), strategy, &|_| {}).unwrap();
            assert_eq!(expected, ranks(&conn), "{strategy:?}");
        }
    }

    /// Checks this through lookup instead of the column. The reader's order is the
    /// purpose of a strategy.
    #[test]
    fn changing_the_strategy_changes_result_order_without_a_rebuild() {
        let (second, _sguard) = second_freq_archive("order_flips");
        let (db, _guard) = built("order_flips", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();

        // BestRank makes 猫 lead for ねこ with rank 3 from the second Dictionary.
        // Priority uses `freq.zip` rank 42 for 猫, so the kana-only ねこ entry with
        // rank 12 leads.
        reindex(&mut conn, &both_dictionaries(), RankingStrategy::BestRank, &|_| {}).unwrap();
        assert_eq!(Some("猫".to_string()), hits(&db, "ねこ")[0].written);

        reindex(&mut conn, &both_dictionaries(), RankingStrategy::Priority, &|_| {}).unwrap();
        assert_eq!(None, hits(&db, "ねこ")[0].written, "the kana-only entry now leads");
    }

    #[test]
    fn disabling_a_frequency_dictionary_reranks_and_re_enabling_restores_it() {
        let (second, _sguard) = second_freq_archive("disable");
        let (db, _guard) = built("disable", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();
        let with_both = ranks(&conn);
        assert_eq!(Ranks { taberu: Some(7), neko: Some(12), cat: Some(3) }, with_both);

        reindex(&mut conn, &[FREQ_A.to_string()], RankingStrategy::BestRank, &|_| {}).unwrap();

        assert_eq!(
            Ranks { taberu: Some(7), neko: None, cat: Some(42) },
            ranks(&conn),
            "a disabled dictionary stops counting, and its ねこ is not a vote"
        );
        assert_eq!(3, claims_of(&conn, 3), "its stored numbers stay exactly where they are");

        reindex(&mut conn, &both_dictionaries(), RankingStrategy::BestRank, &|_| {}).unwrap();

        assert_eq!(with_both, ranks(&conn), "so re-enabling it needs no re-import");
    }

    #[test]
    fn a_word_no_enabled_dictionary_ranks_is_left_unranked() {
        let (db, _guard) = built("nothing_enabled", &[fixture("freq.zip")]);
        let mut conn = Connection::open(&db).unwrap();
        assert_eq!(Some(7), ranks(&conn).taberu);

        reindex(&mut conn, &[], RankingStrategy::BestRank, &|_| {}).unwrap();

        assert_eq!(Ranks { taberu: None, neko: None, cat: None }, ranks(&conn));
        assert!(
            hits(&db, "食べる")[0].freq.is_none(),
            "and the lookup reads the NULL that leaves `score` on DEFAULT_FREQ"
        );
        assert_eq!(3, claims_of(&conn, 2), "nothing was deleted, only not counted");
    }

    #[test]
    fn a_name_no_installed_dictionary_answers_to_is_ignored() {
        let (db, _guard) = built("unknown_name", &[fixture("freq.zip")]);
        let mut conn = Connection::open(&db).unwrap();

        reindex(
            &mut conn,
            &["Never Installed".to_string(), FREQ_A.to_string()],
            RankingStrategy::Priority,
            &|_| {},
        )
        .unwrap();

        assert_eq!(vec![2], recorded(&conn).unwrap().order);
        assert_eq!(Some(7), ranks(&conn).taberu, "the installed one still ranks");
    }

    /// The transaction provides the atomicity guarantee. This test aborts after
    /// `猫`, the last `term` row in the restamp. Four rows have changed when the
    /// trigger fires.
    #[test]
    fn a_failure_part_way_through_a_reindex_leaves_every_rank_where_it_was() {
        let (second, _sguard) = second_freq_archive("atomic");
        let (db, _guard) = built("atomic", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();
        let before = ranks(&conn);
        conn.execute_batch(
            "CREATE TEMP TRIGGER burst BEFORE UPDATE ON term WHEN NEW.surface = '猫'
             BEGIN SELECT RAISE(ABORT, 'burst'); END;",
        )
        .unwrap();

        let err = reindex(&mut conn, &[], RankingStrategy::BestRank, &|_| {})
            .expect_err("an aborted update must fail the reindex");

        assert!(format!("{err:#}").contains("burst"), "got: {err:#}");
        assert_eq!(before, ranks(&conn), "every rank must be back where it was");
        assert_eq!(
            vec![2, 3],
            recorded(&conn).unwrap().order,
            "and the recorded reduction must roll back with them"
        );
    }

    /// The popup reports a value. It does not compute one. The leading enabled
    /// Dictionary supplies the value, regardless of the strategy that orders
    /// results.
    #[test]
    fn the_popup_reports_the_leading_dictionarys_own_number_under_every_strategy() {
        let (second, _sguard) = second_freq_archive("popup_number");
        let (db, _guard) = built("popup_number", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();

        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            reindex(&mut conn, &both_dictionaries(), strategy, &|_| {}).unwrap();
            let card = card_for(&db, "ねこ", Some("猫"));
            assert_eq!(Some(42), card.freq, "freq.zip leads and says 42: {strategy:?}");
            assert_eq!(
                Some(&"42".to_string()),
                crate::anki::fields_from_card(&card, &card.blocks).get("frequency"),
                "and a mined note carries the same number: {strategy:?}"
            );
        }

        assert_eq!(
            Some(22),
            ranks(&conn).cat,
            "the median rank really is a different number, so 42 is not simply the only one"
        );
    }

    #[test]
    fn reordering_the_frequency_list_changes_the_number_the_popup_reports() {
        let (second, _sguard) = second_freq_archive("popup_order");
        let (db, _guard) = built("popup_order", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();
        assert_eq!(Some(42), card_for(&db, "ねこ", Some("猫")).freq);

        reindex(
            &mut conn,
            &[FREQ_B.to_string(), FREQ_A.to_string()],
            RankingStrategy::BestRank,
            &|_| {},
        )
        .unwrap();

        assert_eq!(
            Some(3),
            card_for(&db, "ねこ", Some("猫")).freq,
            "the second dictionary leads now, so its own 3 is what is shown"
        );
    }

    #[test]
    fn a_headword_no_enabled_dictionary_ranks_reports_no_number_at_all() {
        let (db, _guard) = built("popup_unranked", &[fixture("freq.zip")]);
        assert_eq!(
            None,
            card_for(&db, "ねこ", None).freq,
            "freq.zip has no kana-only ねこ, and an absent claim is not a number"
        );
    }
}
