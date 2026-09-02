//! Read-only mmap'd SQLite.

use crate::dict::gloss::{GlossDoc, Kind, NodeId};
use crate::dict::media::{self, Intrinsic, MediaFormat, MediaKey, Missing, Surface};
use crate::dict::pitch::{Accent, PitchClaim, Position};
use crate::dict::reindex;
use crate::dict::sheet::{self, Sheet};
use crate::lookup::model::{Dictionary, Entry, TermRow};
use crate::present::DictInfo;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

pub struct SqliteDictionary {
    conn: Connection,
    gloss: RefCell<GlossCache>,
    /// One dictionary's compiled `styles.css`, compiled on first use.
    ///
    /// Per process, not per hover and not on disk. The corpus carries 174 KB
    /// of CSS across 14 dictionaries, so caching a compiled form in the
    /// database would buy a millisecond once per process and cost every
    /// matcher fix a dictionary rebuild - the same trade ticket 02 made for
    /// the tree itself. `None` records "this dictionary ships none", so a
    /// dictionary without a stylesheet costs one query for the whole
    /// process rather than one per hover.
    sheets: RefCell<HashMap<i64, Option<Rc<Sheet>>>>,
    /// The enabled frequency dictionaries, highest priority first, exactly as
    /// the database records the reduction its Frequency ranks came from
    /// ([`reindex::Reduction`]).
    ///
    /// Read from the file rather than from config, so the Reported frequency
    /// the popup prints comes from a dictionary the ranking in this file
    /// actually consulted. Read once: it is a handful of ids, and a reindex
    /// commits and the daemon reloads, so it cannot change under an open
    /// reader. Empty means no frequency dictionary is enabled, and then no
    /// Reported frequency is looked up at all.
    freq_order: Vec<i64>,
}

/// Must match `dict::build::SCHEMA_VERSION`.
const EXPECTED_SCHEMA_VERSION: i64 = 4;

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
    by_id: HashMap<i64, Cached>,
    order: VecDeque<i64>,
}

/// One cached record: the dictionary it belongs to, its headword's Reported
/// frequency, and the tree.
///
/// The Reported frequency rides along because it is read with the record and
/// is as stable as the record is: it changes when a reindex commits, and a
/// reindex is followed by a reload.
#[derive(Clone)]
struct Cached {
    dict_id: i64,
    reported_freq: Option<i64>,
    doc: Arc<GlossDoc>,
}

impl GlossCache {
    fn get(&self, entry_id: i64) -> Option<Cached> {
        self.by_id.get(&entry_id).cloned()
    }

    fn put(&mut self, entry_id: i64, record: Cached) {
        if self.by_id.insert(entry_id, record).is_none() {
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
        let conn = open_checked(path)?;
        let freq_order = reindex::recorded(&conn)?.order;
        Ok(SqliteDictionary {
            conn,
            gloss: RefCell::new(GlossCache::default()),
            sheets: RefCell::new(HashMap::new()),
            freq_order,
        })
    }

    /// One asset's recorded intrinsic size, for the sizing pass.
    ///
    /// The whole point of recording width, height and aspect at extraction
    /// time: this is a `WITHOUT ROWID` primary-key probe that reads four
    /// small columns and never touches a blob page, so laying out an image
    /// costs no decode and no seek into the asset bytes. 99 807 census
    /// image nodes declare no size of their own, and 字通 averages more
    /// than four image nodes per term row, so this runs tens of times per
    /// hover.
    ///
    /// `None` is the honest answer for an asset the build could not store,
    /// and it is what makes the `alt`-text fallback fire.
    pub fn media_size(&self, key: &MediaKey) -> Result<Option<Intrinsic>> {
        read_media_size(&self.conn, key)
    }

    /// Every image asset one parsed tree names, sized.
    ///
    /// A flat sweep of the arena rather than a tree descent
    /// (`GlossDoc::all_nodes`), because an image node's depth is irrelevant
    /// here and a sweep cannot recurse. Distinct paths only: 三省堂 repeats
    /// one gaiji several times in a row, and a duplicate would be a second
    /// query for an answer already in hand.
    ///
    /// Total: a store fault sizes no image rather than failing the lookup.
    /// That is the same ladder a missing row takes - `alt` text, then a
    /// placeholder box - and losing a hover over one unreadable asset would
    /// be the worse answer.
    fn media_sizes(&self, dict_id: i64, doc: &GlossDoc) -> Vec<(String, Intrinsic)> {
        let mut out: Vec<(String, Intrinsic)> = Vec::new();
        for (id, node) in doc.all_nodes().iter().enumerate() {
            if node.kind != Kind::Image {
                continue;
            }
            let Some(path) = doc
                .attr_of(id as NodeId, "path")
                .and_then(|v| doc.scalar_str(v))
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            if out.iter().any(|(seen, _)| seen == path) {
                continue;
            }
            let key = MediaKey::new(dict_id, path);
            if let Ok(Some(size)) = read_media_size(&self.conn, &key) {
                out.push((path.to_string(), size));
            }
        }
        out
    }

    /// Folds the dictionary's own `styles.css` into one freshly parsed tree.
    ///
    /// Between the parse and the cache, so a cache hit pays nothing: the
    /// stored record is the merged one, and every reader downstream - the
    /// popup's layout pass, the plain-text walk, the HTML renderer - sees one
    /// style record per node and never learns that CSS exists.
    ///
    /// Total. A store fault, or a stylesheet the matcher makes nothing of,
    /// costs the entry its boxes and never the lookup. That is the same
    /// ladder a missing asset takes, for the same reason: 13 of 52 corpus
    /// dictionaries draw every box here, and none of them is worth a lost
    /// hover.
    fn style(&self, dict_id: i64, doc: &mut GlossDoc) {
        let cached = self.sheets.borrow().get(&dict_id).cloned();
        let sheet = match cached {
            Some(sheet) => sheet,
            None => {
                let compiled = read_sheet(&self.conn, dict_id).map(Rc::new);
                self.sheets.borrow_mut().insert(dict_id, compiled.clone());
                compiled
            }
        };
        if let Some(sheet) = sheet {
            sheet::apply(doc, &sheet);
        }
    }
}

/// A read-only handle onto the media store alone.
///
/// Separate from [`SqliteDictionary`] because the two are read from
/// different threads: the worker owns the dictionary and the bin's painter
/// owns this. A `Connection` is not `Sync`, so the painter needs its own -
/// and it wants nothing else the dictionary carries, neither the term index
/// nor the parsed-tree cache.
pub struct MediaStore {
    conn: Connection,
}

impl MediaStore {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(MediaStore { conn: open_checked(path)? })
    }

    /// One asset's recorded intrinsic size.
    pub fn size(&self, key: &MediaKey) -> Result<Option<Intrinsic>> {
        read_media_size(&self.conn, key)
    }

    /// One asset's encoded bytes and format, for a decode at paint time.
    ///
    /// The blobs live in their own table so that this - and only this - is
    /// the query that pages them in. `size` above reads the same row's
    /// small columns and joins nothing, so laying an image out never
    /// touches an asset's bytes.
    pub fn blob(&self, key: &MediaKey) -> Result<Option<(MediaFormat, Vec<u8>)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.format, b.bytes FROM media m \
             JOIN media_blob b ON b.blob_id = m.blob_id \
             WHERE m.dict_id = ?1 AND m.path = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![key.dict_id, &key.path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .optional()
            .with_context(|| format!("reading the bytes of media {key}"))?;
        Ok(row.and_then(|(format, bytes)| Some((MediaFormat::parse(&format)?, bytes))))
    }

    /// One asset's pixels, decoded.
    ///
    /// `at` is the pixel size a **vector** rasterizes at, straight through
    /// to [`media::decode`] - `Tint::Raster`'s pair when the scene asked
    /// for a tinted mask, and `None` otherwise, which takes the asset's
    /// own intrinsic size.
    ///
    /// Total: every way this can fail is a [`Missing`] arm, because a
    /// dictionary's broken asset must cost the popup its `alt` text and
    /// never a frame. The bin caches the answer either way - a key that
    /// cannot paint must not be re-read and re-decoded once per frame.
    pub fn surface(&self, key: &MediaKey, at: Option<(u32, u32)>) -> Result<Surface, Missing> {
        match self.blob(key) {
            Err(e) => Err(Missing::Unavailable(format!("{e:#}"))),
            Ok(None) => Err(Missing::NotStored),
            Ok(Some((format, bytes))) => {
                media::decode(format, &bytes, at).map_err(Missing::Undecodable)
            }
        }
    }
}

/// Opens read-only and refuses a database this build does not understand.
///
/// The version gate is the whole contract: `schema_version` 4 means the
/// `entry` record holds raw glossary JSON, the media tables exist,
/// `dict_style` holds the `styles.css` of each dictionary that ships one,
/// `reported_freq` holds each frequency dictionary's own claims, *and*
/// `pitch` holds each pitch dictionary's own accents. So a store that
/// passes here has all five, and no reader below has to ask.
fn open_checked(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening dictionary {}", path.display()))?;
    // 256MB window; OS pages it.
    conn.pragma_update(None, "mmap_size", 268_435_456i64)?;

    let found_version: Option<String> = conn
        .query_row("SELECT v FROM meta WHERE k = 'schema_version'", [], |r| r.get(0))
        .optional()
        .with_context(|| format!("reading meta.schema_version from {}", path.display()))?;
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
    Ok(conn)
}

fn read_media_size(conn: &Connection, key: &MediaKey) -> Result<Option<Intrinsic>> {
    let mut stmt = conn.prepare_cached(
        "SELECT format, width, height, aspect FROM media \
         WHERE dict_id = ?1 AND path = ?2",
    )?;
    let row = stmt
        .query_row(rusqlite::params![key.dict_id, &key.path], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })
        .optional()
        .with_context(|| format!("reading the size of media {key}"))?;
    Ok(row.and_then(|(format, width, height, aspect)| {
        Some(Intrinsic {
            format: MediaFormat::parse(&format)?,
            width: width as f32,
            height: height as f32,
            aspect: aspect as f32,
        })
    }))
}

/// One dictionary's stylesheet, compiled.
///
/// `None` for a dictionary that ships none, and also for a store fault:
/// there is no ladder below "draw no boxes", and the caller caches the
/// answer either way so a fault costs one query rather than one per hover.
/// The compile itself is total and cannot fail.
fn read_sheet(conn: &Connection, dict_id: i64) -> Option<Sheet> {
    let mut stmt = conn.prepare_cached("SELECT css FROM dict_style WHERE dict_id = ?1").ok()?;
    let css: Option<String> =
        stmt.query_row([dict_id], |r| r.get(0)).optional().ok().flatten();
    Some(Sheet::compile(&css?))
}

/// One headword's Reported frequency, found from the entry that records it.
///
/// Priority-first-wins over what the enabled frequency dictionaries stored:
/// among the ones that have this headword, whichever the frequency list puts
/// first, and within one dictionary its reading-scoped claim ahead of its
/// reading-agnostic one - `dict::frequency::lookup_freq`'s own rule, spelled
/// in SQL rather than in a `HashMap`. It is that rule whichever ranking
/// strategy the Frequency ranks were reduced under, because the popup's job
/// is to report a number a reader can look up, and a median of three sources
/// is not one (ARCHITECTURE.md#dictionary-and-lookup).
///
/// One query per rendered entry, both sides of it index probes, and cached
/// beside the tree it belongs to - so a repeated hover costs nothing and a
/// fresh entry costs this next to a glossary parse. The term path itself is
/// untouched: the rank `score` orders by is still read off the hot `term` row
/// with no join, which is the whole reason it is denormalised there.
///
/// `None` for a headword no enabled dictionary ranks, and for a database with
/// no frequency dictionary enabled - which is asked first, so a library with
/// no frequency data runs no query at all.
fn read_reported_freq(conn: &Connection, order: &[i64], entry_id: i64) -> Result<Option<i64>> {
    if order.is_empty() {
        return Ok(None);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT r.dict_id, r.reading IS NULL, r.rank \
         FROM term t JOIN reported_freq r \
           ON r.term = COALESCE(t.written, t.surface) \
         WHERE t.entry_id = ?1 AND (r.reading = t.reading OR r.reading IS NULL)",
    )?;
    let claims = stmt
        .query_map([entry_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, i64>(2)?))
        })
        .with_context(|| format!("reading the reported frequency of entry {entry_id}"))?;

    let mut best: Option<((usize, bool), i64)> = None;
    for claim in claims {
        let (dict_id, agnostic, rank) = claim
            .with_context(|| format!("reading a reported frequency of entry {entry_id}"))?;
        // A dictionary the frequency list does not name is disabled, and a
        // disabled dictionary is not a data point.
        let Some(position) = order.iter().position(|id| *id == dict_id) else { continue };
        let ranked_by = (position, agnostic);
        if best.as_ref().is_none_or(|(seen, _)| ranked_by < *seen) {
            best = Some((ranked_by, rank));
        }
    }
    Ok(best.map(|(_, rank)| rank))
}

/// Every stored Pitch pattern for one headword and reading.
///
/// One index probe per shown card, on `(term, reading)`, and every claim
/// this database holds for that reading whichever dictionary made it: which
/// of them are enabled and in what order is the pitch list's question, and
/// the pitch list is config's (ARCHITECTURE.md#dictionary-and-lookup). A
/// handful of rows come back - the census bounds a reading at four distinct
/// accents over five dictionaries - so the reduction costs less than a
/// second query would.
///
/// The hot term statement is untouched and cannot come here: pitch is per
/// reading, and a `term` row is per surface. This is read once per card the
/// popup builds, not once per entry and not once per surface probe.
fn read_pitch(conn: &Connection, term: &str, reading: &str) -> Result<Vec<PitchClaim>> {
    let mut stmt = conn.prepare_cached(
        "SELECT dict_id, downstep, pattern, nasal, devoice, tags \
         FROM pitch WHERE term = ?1 AND reading = ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![term, reading], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .with_context(|| format!("reading the pitch of {term} / {reading}"))?;

    let mut out = Vec::new();
    for row in rows {
        let (dict_id, downstep, pattern, nasal, devoice, tags) =
            row.with_context(|| format!("reading a pitch pattern of {term} / {reading}"))?;
        // Exactly one of the two position columns is written, so a row with
        // neither is a row no mora can be indexed by and there is nothing to
        // draw from it.
        let position = match (downstep, pattern) {
            (Some(fall), _) => Position::Downstep(u32::try_from(fall).unwrap_or(0)),
            (None, Some(levels)) => Position::Pattern(levels),
            (None, None) => continue,
        };
        out.push(PitchClaim {
            dict_id,
            accent: Accent {
                position,
                nasal: mora_list(&nasal),
                devoice: mora_list(&devoice),
                tags: serde_json::from_str(&tags).unwrap_or_default(),
            },
        });
    }
    Ok(out)
}

/// One stored mora list, as the indices it names.
///
/// Total, like the rest of this read: a column this build cannot decode
/// costs the accent its markers - which this ticket does not draw anyway -
/// and never the card.
fn mora_list(json: &str) -> Vec<u32> {
    serde_json::from_str(json).unwrap_or_default()
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
            if let Some(cached) = self.gloss.borrow().get(id) {
                let media = self.media_sizes(cached.dict_id, &cached.doc);
                out.push(Entry::new(
                    id,
                    cached.dict_id,
                    cached.doc,
                    media,
                    cached.reported_freq,
                ));
                continue;
            }
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                let dict_id: i64 = r.get(0)?;
                let raw = r
                    .get_ref(1)?
                    .as_str()
                    .with_context(|| format!("reading the glossary of entry {id}"))?;
                let mut parsed = GlossDoc::parse(raw);
                self.style(dict_id, &mut parsed);
                let doc = Arc::new(parsed);
                let reported_freq = read_reported_freq(&self.conn, &self.freq_order, id)?;
                self.gloss.borrow_mut().put(
                    id,
                    Cached { dict_id, reported_freq, doc: Arc::clone(&doc) },
                );
                let media = self.media_sizes(dict_id, &doc);
                out.push(Entry::new(id, dict_id, doc, media, reported_freq));
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

    /// Total, and that is the ladder the trait states: a store fault costs
    /// the card its pitch row and never the hover. Nothing below the header
    /// depends on it, so there is nothing to fall back to and nothing worth
    /// losing a lookup over.
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        read_pitch(&self.conn, term, reading).unwrap_or_default()
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
             CREATE TABLE dict_style(dict_id INTEGER PRIMARY KEY, css TEXT NOT NULL);
             CREATE TABLE reported_freq(dict_id INTEGER NOT NULL, term TEXT NOT NULL,
                                        reading TEXT, rank INTEGER NOT NULL);
             CREATE TABLE pitch(dict_id INTEGER NOT NULL, term TEXT NOT NULL,
                                reading TEXT NOT NULL, downstep INTEGER, pattern TEXT,
                                nasal TEXT NOT NULL, devoice TEXT NOT NULL,
                                tags TEXT NOT NULL);
             CREATE INDEX idx_term_surface ON term(surface);
             CREATE INDEX idx_term_entry_id ON term(entry_id);
             CREATE INDEX idx_reported_freq_term ON reported_freq(term);
             CREATE INDEX idx_pitch_term_reading ON pitch(term, reading);",
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
             INSERT INTO meta VALUES ('schema_version','4');",
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
             INSERT INTO meta VALUES ('schema_version','4');",
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
             INSERT INTO meta VALUES ('schema_version','4');",
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

    /// A version-3 database is the one every existing user has, and it must
    /// fail loudly with the rebuild message rather than reading a file that
    /// has neither a `reported_freq` nor a `pitch` table in it.
    #[test]
    fn opening_a_version_three_database_fails_with_the_rebuild_message() {
        let path = fixture_path("opening_a_version_three_database_fails");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','3');");

        // No Debug on the Ok type.
        let err = SqliteDictionary::open(&path)
            .err()
            .expect("opening a version-3 database should fail");
        let msg = err.to_string();
        assert!(msg.contains("schema_version is 3"), "{msg}");
        assert!(msg.contains("expects 4"), "{msg}");
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

    // ---- the media store (ticket 03) ----

    /// A real built database, because the media read path exists to read
    /// what a build wrote.
    fn built_media_db(test_name: &str) -> (PathBuf, TempDbGuard) {
        let path = fixture_path(test_name);
        let guard = TempDbGuard(path.clone());
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/media/media.zip");
        crate::dict::build::build(&[archive], &[], &path, &|_| {}).expect("the fixture builds");
        (path, guard)
    }

    /// The sizing path: what ticket 12 asks during measurement, and the
    /// reason width, height and aspect are columns at all.
    #[test]
    fn the_dictionary_answers_an_assets_recorded_size_without_reading_its_bytes() {
        let (path, _guard) = built_media_db("media_size");
        let dict = SqliteDictionary::open(&path).unwrap();

        let gif = dict
            .media_size(&MediaKey::new(1, "gaiji/four.gif"))
            .unwrap()
            .expect("a stored asset has a size");
        assert_eq!(MediaFormat::Gif, gif.format);
        assert_eq!((9.0, 5.0), (gif.width, gif.height));
        assert_eq!(9.0 / 5.0, gif.aspect);

        // No row means fall back to `alt`, and it must not be an error.
        assert_eq!(None, dict.media_size(&MediaKey::new(1, "gaiji/unused.png")).unwrap());
        assert_eq!(None, dict.media_size(&MediaKey::new(9, "gaiji/four.gif")).unwrap());
    }

    /// The bin's own handle, on the painting thread. Same rows, no term
    /// index and no parsed-tree cache.
    #[test]
    fn the_store_hands_out_bytes_and_the_format_they_are_in() {
        let (path, _guard) = built_media_db("media_blob");
        let store = MediaStore::open(&path).unwrap();

        let (format, bytes) = store
            .blob(&MediaKey::new(1, "gaiji/five.avif"))
            .unwrap()
            .expect("a stored asset has bytes");
        assert_eq!(MediaFormat::Avif, format);
        assert_eq!(Some(MediaFormat::Avif), crate::dict::media::sniff(&bytes));
        assert_eq!(
            Some(480.0),
            crate::dict::media::probe(&bytes).ok().map(|i| i.width),
            "the bytes that came back are the asset, not a truncation",
        );

        assert!(store.blob(&MediaKey::new(1, "gaiji/missing.png")).unwrap().is_none());

        // And the store answers the size too, so a bin can lay out the
        // placeholder box for an asset it cannot paint.
        let svg = store.size(&MediaKey::new(1, "gaiji/ratio.svg")).unwrap().unwrap();
        assert_eq!((MediaFormat::Svg, 100.0, 40.0), (svg.format, svg.width, svg.height));
        assert!(store.size(&MediaKey::new(1, "gaiji/missing.png")).unwrap().is_none());
    }

    /// Every way a paint-time lookup can come up empty is a `Missing` arm,
    /// because a broken asset must cost the popup its `alt` text and never
    /// a frame - and every census format now reaches pixels, so a stored
    /// JPEG is a surface rather than a refusal.
    #[test]
    fn a_paint_time_surface_lookup_is_total() {
        let (path, _guard) = built_media_db("media_surface");
        let store = MediaStore::open(&path).unwrap();

        let png =
            store.surface(&MediaKey::new(1, "gaiji/one.png"), None).expect("a PNG decodes");
        assert_eq!((12, 7), (png.w, png.h));
        assert_eq!(12 * 7 * 4, png.rgba.len());

        let jpeg =
            store.surface(&MediaKey::new(1, "gaiji/three.jpg"), None).expect("a JPEG decodes");
        assert_eq!((23, 11), (jpeg.w, jpeg.h));

        // The vector is the one asset whose pixels are a size the caller
        // picks, and `MediaStore` is the wire that carries it.
        let svg = store
            .surface(&MediaKey::new(1, "gaiji/ratio.svg"), Some((24, 10)))
            .expect("an SVG rasterizes");
        assert_eq!((24, 10), (svg.w, svg.h));

        assert_eq!(
            Err(Missing::NotStored),
            store.surface(&MediaKey::new(1, "gaiji/nope.png"), None).map(|_| ()),
        );
    }

    /// The version gate is the contract that a store which opens has the
    /// media tables in it: ticket 02 took `schema_version` to 3 to cover
    /// both its record change and these tables.
    #[test]
    fn the_media_store_refuses_a_database_this_build_does_not_understand() {
        let path = fixture_path("media_store_version");
        let _guard = TempDbGuard(path.clone());
        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','2');");

        let err = MediaStore::open(&path).err().expect("a version-2 store must be refused");
        assert!(err.to_string().contains("schema_version is 2"), "got: {err}");
    }

    // ---- a dictionary's own styles.css (ticket 17) ----

    /// The whole of the hover path for a stylesheet: the text comes out of
    /// `dict_style`, the matcher compiles it on first use, and the winners
    /// land in the resolved style record the renderer already reads. This is
    /// 明鏡国語辞典's own shape - a box in CSS and not one inline `style`
    /// anywhere - so before this ticket the entry drew no box at all.
    #[test]
    fn a_stored_stylesheet_reaches_the_resolved_style_record() {
        let path = fixture_path("stored_stylesheet_folds");
        let _guard = TempDbGuard(path.clone());
        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'明鏡国語辞典 第三版',0);
             INSERT INTO entry VALUES (1,1,'[{\"type\":\"structured-content\",\"content\":[\
                 {\"tag\":\"span\",\"data\":{\"fbox\":\"1\"},\"content\":\"書き方\"}]}]');
             INSERT INTO term VALUES ('書く','書く','かく','v5k',10,1,1);
             INSERT INTO dict_style VALUES (1,'span[data-sc-fbox] { \
                 padding: 0.1em; border-radius: 0.2em; border-style: solid }');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let entries = d.entries(&[1]).unwrap();
        let doc = &entries[0].gloss;
        let span = (0..doc.all_nodes().len() as NodeId)
            .find(|id| doc.node(*id).tag == crate::dict::gloss::Tag::Span)
            .expect("the span parsed");
        let record: Vec<(String, String)> = doc
            .style(span)
            .iter()
            .map(|(k, v)| (format!("{k:?}"), doc.scalar_str(*v).unwrap_or("?").to_string()))
            .collect();
        assert_eq!(
            vec![
                ("PaddingTop".to_string(), "0.1em".to_string()),
                ("PaddingRight".to_string(), "0.1em".to_string()),
                ("PaddingBottom".to_string(), "0.1em".to_string()),
                ("PaddingLeft".to_string(), "0.1em".to_string()),
                ("BorderRadius".to_string(), "0.2em".to_string()),
                ("BorderStyle".to_string(), "solid".to_string()),
            ],
            record,
            "one entry per property, in the rule's own source order, and the \
             `padding` shorthand expanded into the four longhands it sets",
        );
    }

    /// A dictionary that ships none is untouched, and the tree it hands back
    /// is byte for byte the one the parser produced.
    #[test]
    fn a_dictionary_without_a_stylesheet_folds_nothing() {
        let path = fixture_path("no_stylesheet_folds_nothing");
        let _guard = TempDbGuard(path.clone());
        let glossary = "[{\"type\":\"structured-content\",\"content\":\
             [{\"tag\":\"span\",\"data\":{\"fbox\":\"1\"},\"content\":\"x\"}]}]";
        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO entry VALUES (1,1,'[{\"type\":\"structured-content\",\"content\":[\
                 {\"tag\":\"span\",\"data\":{\"fbox\":\"1\"},\"content\":\"x\"}]}]');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let entries = d.entries(&[1]).unwrap();
        assert_eq!(GlossDoc::parse(glossary), *entries[0].gloss);
    }

    /// The compile happens once per dictionary per process, which is the
    /// whole reason the sheet is cached rather than the tree's boxes. A
    /// second entry off the same dictionary must still be styled, and a
    /// dictionary the cache has already answered "none" for must not be
    /// queried again - both of which this asserts by observing that the
    /// second entry comes out styled after the first has warmed the cache.
    #[test]
    fn the_compiled_sheet_is_reused_across_entries() {
        let path = fixture_path("sheet_cache_reused");
        let _guard = TempDbGuard(path.clone());
        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO entry VALUES (1,1,'[{\"type\":\"structured-content\",\
                 \"content\":{\"tag\":\"span\",\"data\":{\"a\":\"1\"},\"content\":\"one\"}}]');
             INSERT INTO entry VALUES (2,1,'[{\"type\":\"structured-content\",\
                 \"content\":{\"tag\":\"span\",\"data\":{\"a\":\"1\"},\"content\":\"two\"}}]');
             INSERT INTO dict_style VALUES (1,'span[data-sc-a] { margin-top: 3px }');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        for id in [1i64, 2] {
            let entries = d.entries(&[id]).unwrap();
            let doc = &entries[0].gloss;
            let span = (0..doc.all_nodes().len() as NodeId)
                .find(|n| doc.node(*n).tag == crate::dict::gloss::Tag::Span)
                .expect("the span parsed");
            assert_eq!(
                Some("3px"),
                doc.style_of(span, crate::dict::gloss::StyleKey::MarginTop)
                    .and_then(|v| doc.scalar_str(v)),
                "entry {id}",
            );
        }
        assert_eq!(1, d.sheets.borrow().len(), "one compile for one dictionary");
    }

    // ---- pitch (ticket 02) ----

    /// Every field the table holds comes back, markers included, and the
    /// claims come back with the dictionary that made them rather than
    /// filtered or ordered: which are enabled and in what order is the pitch
    /// list's question, and the pitch list is config's
    /// (ARCHITECTURE.md#dictionary-and-lookup).
    #[test]
    fn a_pitch_read_returns_every_claim_with_its_dictionary_and_its_markers() {
        let path = fixture_path("a_pitch_read_returns_every_claim");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'NHK',0);
             INSERT INTO dict VALUES (2,'Sanseido',1);
             INSERT INTO pitch VALUES (1,'合鍵','あいかぎ',0,NULL,'[4]','[2]','[\"名\"]');
             INSERT INTO pitch VALUES (2,'合鍵','あいかぎ',3,NULL,'[]','[]','[]');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let claims = d.pitch_for("合鍵", "あいかぎ");

        assert_eq!(2, claims.len());
        assert_eq!(1, claims[0].dict_id);
        assert_eq!(Position::Downstep(0), claims[0].accent.position);
        assert_eq!(vec![4], claims[0].accent.nasal);
        assert_eq!(vec![2], claims[0].accent.devoice);
        assert_eq!(vec!["名".to_string()], claims[0].accent.tags);
        assert_eq!(2, claims[1].dict_id);
        assert_eq!(Position::Downstep(3), claims[1].accent.position);
        assert!(claims[1].accent.nasal.is_empty());
    }

    /// The `^[HL]+$` form, which the schema permits and no archive in either
    /// of ticket 01's corpora writes. Stored in its own column because the
    /// two forms of `position` share no indexing origin.
    #[test]
    fn a_stored_pattern_comes_back_as_a_pattern_and_not_as_a_downstep() {
        let path = fixture_path("a_stored_pattern_comes_back");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO pitch VALUES (1,'例','れい',NULL,'LHHL','[]','[]','[]');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();
        let claims = d.pitch_for("例", "れい");

        assert_eq!(Position::Pattern("LHHL".to_string()), claims[0].accent.position);
    }

    /// The reading has to match: Yomitan skips a payload whose reading is
    /// not the headword's, and 122 rows in the census carry a reading no
    /// term dictionary emits.
    #[test]
    fn a_pitch_read_for_another_reading_finds_nothing() {
        let path = fixture_path("a_pitch_read_for_another_reading");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO pitch VALUES (1,'扱い','〜あつかい',2,NULL,'[]','[]','[]');
             INSERT INTO meta VALUES ('schema_version','4');",
        );

        let d = SqliteDictionary::open(&path).unwrap();

        assert!(d.pitch_for("扱い", "あつかい").is_empty());
        assert_eq!(1, d.pitch_for("扱い", "〜あつかい").len(), "the odd row is still there");
    }

    /// The hot term statement must not grow a join or a column for pitch:
    /// it runs roughly 25 times per hover and a Pitch pattern is per
    /// reading, read once per shown card instead. Asserted as the plan
    /// SQLite makes of it, which is what "no join" actually means - one
    /// table, one index probe, and neither of the two per-dictionary tables
    /// anywhere in it (ARCHITECTURE.md#dictionary-and-lookup).
    #[test]
    fn the_term_statement_still_plans_as_one_index_probe_with_no_join() {
        let path = fixture_path("the_term_statement_still_plans");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(
            &path,
            "INSERT INTO dict VALUES (1,'d',0);
             INSERT INTO meta VALUES ('schema_version','4');",
        );
        let d = SqliteDictionary::open(&path).unwrap();

        let mut stmt = d
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT surface, written, reading, pos, freq, entry_id, dict_id \
                 FROM term WHERE surface = ?1",
            )
            .unwrap();
        let steps: Vec<String> = stmt
            .query_map(["食べる"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        assert_eq!(1, steps.len(), "one step is no join: {steps:?}");
        assert!(steps[0].contains("idx_term_surface"), "{steps:?}");
        assert!(
            !steps.iter().any(|s| s.contains("pitch") || s.contains("reported_freq")),
            "the per-dictionary tables are off the term path entirely: {steps:?}"
        );
    }
}
