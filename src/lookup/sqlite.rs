//! Read-only SQLite access with a memory map.

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
    /// One Dictionary's compiled `styles.css`, created on first use.
    ///
    /// The cache stays in memory. It serves one process, not one hover. The corpus
    /// has 174 KB of CSS across 14 Dictionaries. A compiled form in the database
    /// would save about one millisecond per process but force every matcher fix
    /// to rebuild a Dictionary. This cache uses the same choice as the tree cache.
    /// The cache stores `None` when a Dictionary has no stylesheet or when reading
    /// or setting up the stylesheet fails. It costs one query for the whole process,
    /// not one query per hover.
    sheets: RefCell<HashMap<i64, Option<Rc<Sheet>>>>,
    /// Enabled frequency Dictionaries in highest-priority order. The database
    /// stores the reduction that produced their Frequency ranks
    /// ([`reindex::Reduction`]).
    ///
    /// The constructor reads this order from the database, not config. The popup
    /// therefore prints a Reported frequency from a Dictionary that the stored
    /// rank order used for its Frequency ranks. The constructor reads only a few
    /// Dictionary IDs. A reindex commits the data and the daemon reloads, so the
    /// order cannot change for an open reader. An empty list means that no
    /// frequency Dictionary is enabled. The reader then skips Reported frequency
    /// lookup.
    freq_order: Vec<i64>,
}

/// This value must match `dict::build::SCHEMA_VERSION`.
const EXPECTED_SCHEMA_VERSION: i64 = 4;

/// Parsed trees that the cache keeps.
///
/// A hover renders at most `MAX_RESULTS` = 10 entries. The cache therefore
/// covers about the last 25 hovers. It lets a dwell re-check, a drill-down, and
/// a collapsed-row swap reuse each parse. The retained heap stays in the low
/// megabytes. `examples/gloss_doc_alloc.rs` measures the cost of one cached
/// entry.
const GLOSS_CACHE_ENTRIES: usize = 256;

/// The database stores raw glossary JSON. The cache parses a tree as needed for a
/// hover. A parser fix can therefore ship as a patch instead of a Dictionary
/// rebuild.
///
/// The cache uses insertion order, not recency. Hovers use a recent-entry window,
/// so the orders nearly match. FIFO adds one item per miss instead of one change
/// per hit.
#[derive(Default)]
struct GlossCache {
    by_id: HashMap<i64, Cached>,
    order: VecDeque<i64>,
}

/// One cached record: its Dictionary, its headword's Reported frequency, and its
/// tree.
///
/// The query reads Reported frequency with the record, so the record stores it.
/// The value has the same lifetime as the record. A reindex changes the value,
/// and a reload follows each reindex.
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

    /// One asset's recorded intrinsic size for the size pass.
    ///
    /// The build records width, height, and aspect at extraction time. This method
    /// uses a `WITHOUT ROWID` primary-key probe to read four small columns. It
    /// never reads a blob page or seeks into asset bytes, so image layout needs no
    /// decode. The census has 99 807 image nodes with no size of their own. 字通
    /// averages more than four image nodes per term row, so this method runs many
    /// times per hover.
    ///
    /// `None` is the correct result when the build could not store an asset. It
    /// makes the `alt`-text fallback run.
    pub fn media_size(&self, key: &MediaKey) -> Result<Option<Intrinsic>> {
        read_media_size(&self.conn, key)
    }

    /// Size every image asset that one parsed tree names.
    ///
    /// Use a flat sweep of the arena instead of a tree descent
    /// (`GlossDoc::all_nodes`). Image depth does not matter here, and a sweep
    /// cannot recurse. Keep distinct paths only. 三省堂 repeats one gaiji several
    /// times in a row, and a duplicate would issue a second query for the same
    /// result.
    ///
    /// This method is total. A store fault returns no image size and no lookup
    /// error. The missing-row path uses `alt` text and then a placeholder box. A
    /// hover can survive one unreadable asset.
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

    /// Merge the Dictionary's `styles.css` into one parsed tree.
    ///
    /// Apply the sheet between the parse and the cache, so a cache hit does no
    /// extra work. The stored record is already merged. Downstream readers
    /// include the popup layout pass, plain-text walk, and HTML renderer. Each
    /// reader sees one style record per node. No reader needs to know about CSS.
    ///
    /// This method is total. A store fault or a sheet that matches no nodes
    /// leaves the Entry without boxes, not the lookup. The missing-asset path
    /// uses the same rule. 13 of 52 census Dictionaries draw every box here. No
    /// such case justifies a lost hover.
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

/// A read-only handle for the media store.
///
/// Keep this handle separate from [`SqliteDictionary`]. The Worker owns the
/// Dictionary, and the bin's painter owns this handle on another thread.
/// `Connection` is not `Sync`, so the painter needs its own connection. It needs
/// no term index or parsed-tree cache.
pub struct MediaStore {
    conn: Connection,
}

impl MediaStore {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(MediaStore { conn: open_checked(path)? })
    }

    /// Return the recorded intrinsic size for one asset.
    pub fn size(&self, key: &MediaKey) -> Result<Option<Intrinsic>> {
        read_media_size(&self.conn, key)
    }

    /// Return one asset's encoded bytes and format for a paint-time decode.
    ///
    /// The blobs have a separate table. This is the only query that reads them.
    /// `size` reads the same row's small columns without a join. Image layout
    /// never reads asset bytes.
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

    /// Return one asset's decoded pixels.
    ///
    /// `at` gives the pixel size for a **vector** asset. This method passes it
    /// directly to [`media::decode`]. `Tint::Raster` provides the pair when the
    /// scene requests a tinted mask. `None` makes the decoder use the asset's
    /// intrinsic size.
    ///
    /// This method is total. Every failure becomes a [`Missing`] variant. A
    /// broken Dictionary asset makes the popup use its `alt` text. It does not
    /// cost a frame. The bin caches both results. It must not read and decode a
    /// key that cannot paint once per frame.
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

/// Open a read-only connection and reject a database that this build does not
/// understand.
///
/// The version gate defines the contract. `schema_version` 4 means that the
/// `entry` record has raw glossary JSON and that the media tables exist.
/// `dict_style` holds each Dictionary's `styles.css` when present.
/// `reported_freq` holds each frequency Dictionary's claims. `pitch` holds each
/// pitch Dictionary's accents. A store that passes the gate has all five. Readers
/// below need no extra checks.
fn open_checked(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening dictionary {}", path.display()))?;
    // The OS pages a 256 MB memory window.
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

/// One Dictionary's compiled stylesheet.
///
/// Return `None` when a Dictionary has no stylesheet or when reading or setting up
/// the stylesheet fails. The fallback draws no boxes. The caller caches either
/// result, so one fault costs one query for the process, not one query per hover.
/// Sheet compilation cannot fail.
fn read_sheet(conn: &Connection, dict_id: i64) -> Option<Sheet> {
    let mut stmt = conn.prepare_cached("SELECT css FROM dict_style WHERE dict_id = ?1").ok()?;
    let css: Option<String> =
        stmt.query_row([dict_id], |r| r.get(0)).optional().ok().flatten();
    Some(Sheet::compile(&css?))
}

/// Read one headword's Reported frequency from the Entry record.
///
/// The frequency list gives priority to its first enabled Dictionary. Among
/// Dictionaries that have the headword, the first one in that list wins. Within
/// one Dictionary, a reading-specific claim wins over a reading-agnostic claim.
/// This matches `dict::frequency::lookup_freq`, but SQL applies the rule here
/// instead of a `HashMap`. The rule stays the same after a Ranking strategy
/// reduces Frequency ranks. The popup reports a number that a reader can look
/// up. The displayed number comes from one Dictionary, not a value computed from
/// several Dictionaries (ARCHITECTURE.md#dictionary-and-lookup).
///
/// The method runs one query per rendered Entry. Both sides use index probes, and
/// the result is cached with its parsed tree. A repeated hover then does no work.
/// The term path stays unchanged. The hot `term` row supplies `freq`, from which
/// the ranker computes `score`. The query uses no join because `term.freq` is
/// denormalized.
///
/// Return `None` when no enabled Dictionary ranks the headword. Return it also
/// when no frequency Dictionary is enabled. This check lets a Dictionary without
/// frequency data run no query.
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
        // A Dictionary absent from the frequency list is disabled. A disabled
        // Dictionary is not a data point.
        let Some(position) = order.iter().position(|id| *id == dict_id) else { continue };
        let ranked_by = (position, agnostic);
        if best.as_ref().is_none_or(|(seen, _)| ranked_by < *seen) {
            best = Some((ranked_by, rank));
        }
    }
    Ok(best.map(|(_, rank)| rank))
}

/// Return every stored Pitch pattern for one headword and reading.
///
/// Use one index probe per shown card on `(term, reading)`. Return every claim
/// that this database has for that reading, from each Dictionary. The pitch list
/// in config decides which Dictionaries are enabled and how it orders them
/// (ARCHITECTURE.md#dictionary-and-lookup). The query returns a few rows. The
/// census finds at most four distinct accents across five Dictionaries, so the
/// reduction costs less than another query.
///
/// Keep this query off the hot term statement. Pitch belongs to a reading, but a
/// `term` row belongs to a surface. Read it once for each card that the popup
/// builds, not once for each Entry or surface probe.
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
        // The database writes exactly one of the two position columns. A row with
        // neither value has no indexable mora and supplies nothing to draw.
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

/// Convert one stored mora list to its indices.
///
/// This conversion is total. If this build cannot decode the column, the accent
/// loses its markers. This build does not draw those markers, and the card stays
/// visible.
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

    /// The cache handles the glossary parse on the hover path. SQLite provides raw
    /// TEXT as `&str`, so this path does not first copy it to a `String`. The old
    /// `Vec<Sense>` path copied it without need.
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

    /// This method is total. A store fault returns no Pitch pattern, not an error
    /// for the hover. Nothing below the card header depends on this value, so the
    /// lookup can continue.
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        read_pitch(&self.conn, term, reading).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::Dictionary;
    use std::path::{Path, PathBuf};

    /// Removes the temporary database file when the guard drops.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Returns a path that is unique for each process and test.
    fn fixture_path(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_sqlite_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Creates the real schema and then inserts seed data.
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

    /// The cache prevents a second parse on a second read. The test must receive
    /// the same parsed tree.
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

    /// Real data often has null columns.
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

    /// Users with version-3 databases need a rebuild. The reader rejects the
    /// file and returns the rebuild message. The file lacks `reported_freq` and
    /// `pitch` tables.
    #[test]
    fn opening_a_version_three_database_fails_with_the_rebuild_message() {
        let path = fixture_path("opening_a_version_three_database_fails");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','3');");

        // The Ok type does not implement Debug.
        let err = SqliteDictionary::open(&path)
            .err()
            .expect("opening a version-3 database should fail");
        let msg = err.to_string();
        assert!(msg.contains("schema_version is 3"), "{msg}");
        assert!(msg.contains("expects 4"), "{msg}");
        assert!(msg.to_lowercase().contains("rebuild"), "{msg}");
    }

    /// The reader must reject this database.
    #[test]
    fn open_fails_when_schema_version_does_not_match() {
        let path = fixture_path("open_fails_when_schema_version_does_not_match");
        let _guard = TempDbGuard(path.clone());

        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','1');");


        // The Ok type does not implement Debug.
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

    // ---- the media store ----

    /// Creates a real built database. The media read path must read data that the
    /// build wrote.
    fn built_media_db(test_name: &str) -> (PathBuf, TempDbGuard) {
        let path = fixture_path(test_name);
        let guard = TempDbGuard(path.clone());
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/media/media.zip");
        crate::dict::build::build(&[archive], &[], &path, &|_| {}).expect("the fixture builds");
        (path, guard)
    }

    /// This is the size path that measurement asks for. It explains why width,
    /// height, and aspect are columns.
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

        // No row means that the caller uses `alt`. This case is not an error.
        assert_eq!(None, dict.media_size(&MediaKey::new(1, "gaiji/unused.png")).unwrap());
        assert_eq!(None, dict.media_size(&MediaKey::new(9, "gaiji/four.gif")).unwrap());
    }

    /// The bin owns this handle on the paint thread. It reads the same rows
    /// without the term index or parsed-tree cache.
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

        // The store also returns the size. The bin can then lay out the
        // placeholder box for an asset it cannot paint.
        let svg = store.size(&MediaKey::new(1, "gaiji/ratio.svg")).unwrap().unwrap();
        assert_eq!((MediaFormat::Svg, 100.0, 40.0), (svg.format, svg.width, svg.height));
        assert!(store.size(&MediaKey::new(1, "gaiji/missing.png")).unwrap().is_none());
    }

    /// Every empty paint-time lookup produces a `Missing` variant. A broken
    /// asset makes the popup use its `alt` text. It does not cost a frame. Every
    /// census format now reaches pixels, so a stored JPEG must return a surface.
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

        // The vector is the one asset whose pixel size the caller chooses. `MediaStore`
        // carries that size to the decoder.
        let svg = store
            .surface(&MediaKey::new(1, "gaiji/ratio.svg"), Some((24, 10)))
            .expect("an SVG rasterizes");
        assert_eq!((24, 10), (svg.w, svg.h));

        assert_eq!(
            Err(Missing::NotStored),
            store.surface(&MediaKey::new(1, "gaiji/nope.png"), None).map(|_| ()),
        );
    }

    /// The version gate guarantees that an opened store has its media tables. The
    /// current schema is version 4. This test confirms that version 2 fails before
    /// the reader uses those tables.
    #[test]
    fn the_media_store_refuses_a_database_this_build_does_not_understand() {
        let path = fixture_path("media_store_version");
        let _guard = TempDbGuard(path.clone());
        seed_fixture_db(&path, "INSERT INTO meta VALUES ('schema_version','2');");

        let err = MediaStore::open(&path).err().expect("a version-2 store must be refused");
        assert!(err.to_string().contains("schema_version is 2"), "got: {err}");
    }

    // ---- a dictionary's own styles.css ----

    /// This test covers the stylesheet path. The reader gets CSS from `dict_style`,
    /// compiles it on first use, and writes matched properties to the resolved
    /// style record that the renderer already reads. 明鏡国語辞典 uses a CSS box
    /// instead of an inline `style`, so before this support, the renderer drew no
    /// box for the Entry.
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

    /// A Dictionary without a stylesheet stays unchanged. The returned tree matches
    /// the parser output byte for byte.
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

    /// The cache compiles one sheet per Dictionary, not per tree. A second Entry
    /// from the same Dictionary must still get styles. This test checks the
    /// second Entry after the first Entry fills the cache and confirms one cached
    /// sheet.
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

    // ---- pitch ----

    /// This read returns every table field with markers. Each claim keeps its
    /// Dictionary. The read does not filter or order claims. The pitch list owns
    /// those choices (ARCHITECTURE.md#dictionary-and-lookup).
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

    /// The schema permits the `^[HL]+$` form, but no archive in either corpus writes
    /// it. The database stores it in a separate column because the two `position`
    /// forms use different index origins.
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

    /// The reading must match. Yomitan skips a payload with a different reading.
    /// The census has 122 rows with a reading that no Dictionary in the terms list
    /// emits.
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

    /// Keep Pitch out of the hot term statement. The statement runs about 25 times
    /// per hover, but a Pitch pattern belongs to one reading. Read it once per
    /// shown card.
    ///
    /// This test checks SQLite's query plan. One table and one index probe prove
    /// "no join", and neither per-Dictionary table appears
    /// (ARCHITECTURE.md#dictionary-and-lookup).
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
