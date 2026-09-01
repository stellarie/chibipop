//! The Reindex: Frequency ranks recomputed from stored Reported frequencies.
//!
//! One in-place transaction over rows that are already here. Nothing in this
//! module reads an archive, builds a file beside the live one, or renames
//! anything - that is [`crate::dict::build::build`], and it exists for
//! archive reads only (ADR-0005). The promoted database is already stamped
//! `PRAGMA journal_mode = WAL` precisely because it is read while being
//! written, so a reader keeps seeing the old ranking until the commit and
//! the new one afterwards, and the daemon picks it up through the existing
//! `reload` control-socket verb.

use crate::dict::frequency::{lookup_freq, reduce, FreqSource, FreqTable, RankingStrategy};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::BTreeSet;

/// The `meta` key holding the enabled frequency dictionaries, in order.
const ORDER_KEY: &str = "frequency_order";

/// The `meta` key holding the strategy the Frequency ranks were reduced under.
const STRATEGY_KEY: &str = "frequency_strategy";

/// What this database's Frequency ranks were reduced from.
///
/// Recorded in `meta` rather than re-derived from config, because `term.freq`
/// is derived state and the popup has to agree with the inputs it was
/// *actually* derived from: a reader that took the order out of config would
/// print the Reported frequency of a dictionary the ranking in the file never
/// consulted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reduction {
    /// The enabled frequency dictionaries' `dict_id`s, highest priority
    /// first. A dictionary absent from this list is disabled: it contributes
    /// nothing to any rank, and its stored claims stay exactly where they
    /// are, so re-enabling it costs a reindex and never a re-import.
    pub order: Vec<i64>,
    pub strategy: RankingStrategy,
}

/// Recomputes every Frequency rank from the Reported frequencies already
/// stored, in place, inside one transaction.
///
/// `enabled` names the frequency dictionaries the user has switched on, in
/// the order the frequency list puts them in - position is priority within
/// that role and nothing else. A name no installed dictionary answers to is
/// ignored, so a config that still names an unplugged dictionary is not an
/// error. `strategy` is the rule that reduces the claims of the dictionaries
/// that have a word into the one rank `term.freq` carries; a word none of
/// them has ends as `NULL`, which is what leaves `score` on its
/// `DEFAULT_FREQ` fallback.
///
/// Returns the number of `term` rows restamped. A failure part way through
/// leaves every rank at its previous value: the transaction is the whole
/// guarantee, and nothing here is written outside it.
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
    // No `ANALYZE`: a reindex rewrites the values in one unindexed column
    // and moves no row, so nothing the planner samples has changed.
    tx.commit().context("committing the reindex")?;
    Ok(restamped)
}

/// The reduction this database's Frequency ranks were computed under.
///
/// Both keys absent is an empty order under the default strategy, and that
/// is the truth rather than a fallback: every build writes both, so absence
/// means this database has no frequency dictionary enabled and therefore
/// nothing to report. A value that cannot be read *is* an error - it is a
/// hand-edited or corrupt record, and ranking by something else without
/// saying so would be worse than refusing.
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

/// Records the reduction the rows now in `term` were stamped under.
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

/// Stores one frequency dictionary's own claims.
///
/// The keys are `FreqTable`'s keys, spelled out: term plus optional reading,
/// so `lookup_freq`'s reading-scoped-then-reading-agnostic rule reads back
/// off these rows exactly as it reads off the table they came from.
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

/// Brings the stored Reported frequencies in line with the frequency
/// archives the library holds, and records the reduction that follows.
///
/// The archive-driven half of the story, for an import or a removal rather
/// than a settings change: `sources` is every frequency archive in effect, so
/// a dictionary it names gets a `dict` row and its claims stored, and one it
/// no longer names loses them. The strategy the database already records is
/// preserved - only the archives changed - and a newly named dictionary lands
/// at the bottom of the order, which is where an import belongs.
pub(crate) fn sync_reported(tx: &Transaction, sources: &[FreqSource]) -> Result<Reduction> {
    let strategy = recorded(tx)?.strategy;
    let mut held = frequency_dictionaries(tx)?;
    let mut order = Vec::with_capacity(sources.len());

    for source in sources {
        let dict_id = match held.iter().position(|(_, name)| *name == source.name) {
            Some(found) => held.remove(found).0,
            // An archive supplying terms as well as frequency already has
            // its `dict` row, made when its definitions were inserted, and
            // holds no claims yet - so it is not among the frequency
            // dictionaries and must not be given a second row wearing the
            // same name. Only a name the database has never heard of gets
            // one.
            None => match unclaimed_dict_row(tx, &source.name, &order)? {
                Some(existing) => existing,
                None => new_dict_row(tx, &source.name)?,
            },
        };
        drop_reported(tx, dict_id)?;
        store_reported(tx, dict_id, &source.table)?;
        order.push(dict_id);
    }

    // Whatever is left held claims from an archive the library no longer
    // lists. A dictionary that also supplies terms keeps its row and its
    // entries and loses only its claims; one that supplied nothing else is
    // gone entirely.
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

/// Restamps every Frequency rank from what one reduction says this database
/// holds - the half a settings change and an archive change share.
pub(crate) fn restamp_from_stored(
    tx: &Transaction,
    reduction: &Reduction,
    on_progress: &dyn Fn(&str),
) -> Result<u64> {
    let ranks = reduce(&stored_tables(tx, &reduction.order)?, reduction.strategy);
    restamp(tx, &ranks, on_progress)
}

/// Restamps `term.freq` from one reduced table, and reports how many rows it
/// wrote.
///
/// Every row, not just the ones that change: the strategy, the order and the
/// enabled set are all inputs, so a row whose rank is unchanged under the new
/// reduction is a row this pass has confirmed rather than one it may skip.
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
        // The headword a frequency archive names is the written form when
        // there is one, and the reading when the headword is kana-only.
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

/// Each dictionary's stored claims, in the order it was named.
///
/// One scan of `reported_freq` rather than one query per dictionary, and the
/// disabled dictionaries' rows are dropped as they go past: a corpus holds a
/// handful of frequency dictionaries and there is nothing an index adds to
/// reading all of one of them.
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

/// The `dict` rows the frequency list names, in the order it names them.
///
/// A name no installed dictionary answers to is dropped, because a config may
/// go on naming a dictionary the user has unplugged and an absent dictionary
/// reports nothing. A name two installed dictionaries share contributes both,
/// in `dict_id` order: `dict.name` is a title and two editions can share one.
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

/// Every dictionary this database holds Reported frequencies for, by name.
///
/// The union of what `reported_freq` names and what the recorded order names,
/// because either alone misses a case: an archive whose banks carry no `freq`
/// row at all stores nothing and would look uninstalled, and a dictionary the
/// user has disabled is not in the order but still holds its claims.
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

/// The `dict` row this name already has and no earlier source has taken.
///
/// A Dictionary supplying terms and frequency data is inserted once, by the
/// pass that reads its definitions, and its claims belong under that row.
/// `taken` holds the ids the sources before this one resolved to, so two
/// editions sharing a title still get one row each rather than both landing
/// on the first.
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

/// A `dict` row for a dictionary this database has not seen before.
///
/// A frequency dictionary is a dictionary: it is what the user orders and
/// enables, its claims are stored under its own `dict_id`, and removing it
/// drops them through the same [`crate::dict::build::DICT_KEYED`] walk every
/// other dictionary-keyed table takes. `priority` continues the builder's own
/// relation - one below `dict_id` - and orders nothing here, because priority
/// within the frequency role is the recorded order and not this column.
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

    /// `freq.zip`'s own title, and the one the second archive gets.
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

    /// A second frequency archive, so that "per dictionary" has two
    /// dictionaries to be per.
    ///
    /// Written here rather than committed because the numbers *are* the
    /// fixture, and all three of the facts they carry have to be readable at
    /// a glance: it disagrees with `freq.zip` about both headwords they
    /// share, its 猫 is the commoner of the two claims while its 食べる is
    /// the rarer, and it ranks a headword `freq.zip` does not have at all.
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

    /// A real built database, so a reindex is exercised against the rows the
    /// builder actually writes.
    fn built(test: &str, freqs: &[PathBuf]) -> (PathBuf, TempFileGuard) {
        let out = scratch(test, "chibipop.sqlite");
        let guard = TempFileGuard(out.clone());
        crate::dict::build::build(&[fixture("terms.zip")], freqs, &out, &|_| {}).unwrap();
        (out, guard)
    }

    /// The Frequency rank each of the fixture's three headwords carries.
    ///
    /// Named by the surface that reaches exactly one `term` row: `食べる`
    /// and `猫` are the written forms, and the kana-only `ねこ` entry is the
    /// one whose row has no written form at all.
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

    /// Every dictionary's stored claim about one headword, by `dict_id`.
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

    /// A lookup against the built database, through the real engine.
    ///
    /// No deconjugation rules: every input here is already a plain form, and
    /// the deconjugator seeds that form on its own.
    fn hits(db: &Path, text: &str) -> Vec<Hit> {
        let dict = SqliteDictionary::open(db).expect("the built database opens");
        LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap()
    }

    /// The card one headword gets in the popup, as `present::build` makes it.
    fn card_for(db: &Path, text: &str, written: Option<&str>) -> Card {
        let dict = SqliteDictionary::open(db).expect("the built database opens");
        let found = LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap();
        let installed = dict.dicts().unwrap();
        // A config naming nothing enables every dictionary it finds, which
        // is the state a fresh install resolves to.
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

        // `freq.zip` says 食べる 7 and 猫/ねこ 42 and has no ねこ at all; the
        // second says 500, 3 and 12. So best rank takes the lower of each
        // pair, priority takes `freq.zip`'s, and the median of two ranks is
        // the midpoint - except for ねこ, which only one dictionary has and
        // which every rule therefore answers the same way.
        for (strategy, expected) in [
            (RankingStrategy::BestRank, Ranks { taberu: Some(7), neko: Some(12), cat: Some(3) }),
            (RankingStrategy::Priority, Ranks { taberu: Some(7), neko: Some(12), cat: Some(42) }),
            (RankingStrategy::Median, Ranks { taberu: Some(253), neko: Some(12), cat: Some(22) }),
        ] {
            reindex(&mut conn, &both_dictionaries(), strategy, &|_| {}).unwrap();
            assert_eq!(expected, ranks(&conn), "{strategy:?}");
        }
    }

    /// Asserted through a lookup rather than by reading the column, because
    /// the order a reader sees is what a strategy is for.
    #[test]
    fn changing_the_strategy_changes_result_order_without_a_rebuild() {
        let (second, _sguard) = second_freq_archive("order_flips");
        let (db, _guard) = built("order_flips", &[fixture("freq.zip"), second]);
        let mut conn = Connection::open(&db).unwrap();

        // Best rank makes 猫 the commonest thing ねこ can be, at the second
        // dictionary's 3. Priority takes `freq.zip`'s 42 for 猫 instead, and
        // that puts the kana-only ねこ - which only the second dictionary
        // ranks, at 12 - ahead of it.
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

    /// The transaction is the whole atomicity guarantee, so it is worth an
    /// aborted pass to prove it: `猫` is the last `term` row the restamp
    /// reaches, so four rows have already been rewritten when this fires.
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

    /// The popup reports, it does not compute: the number on screen is what
    /// the leading enabled dictionary published, whichever rule ordered the
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
