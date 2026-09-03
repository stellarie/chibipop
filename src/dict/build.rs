//! Dictionary build schema and writer.

use crate::dict::archive::{
    for_each_media, for_each_meta_row, for_each_row, read_index, read_styles_css, TermBanks,
};
use crate::dict::frequency::{
    self, lookup_freq, merge_freq_row, FreqSource, FreqTable, RankingStrategy,
};
use crate::dict::gloss::{renders_text, GlossDoc, Kind, NodeId};
use crate::dict::media::{self, Intrinsic};
use crate::dict::pitch;
use crate::dict::reindex;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{SystemTime, UNIX_EPOCH};

/// Version 4 adds per-Dictionary claims to `reported_freq` and adds the `pitch` table.
/// `reported_freq` keeps each frequency Dictionary claim separate. The build does not merge
/// claims globally.
/// Version 4 requires one rebuild because the library directory keeps the source archives.
/// The rebuild reads those archives again. It does not re-import them.
const SCHEMA_VERSION: i64 = 4;
#[cfg(test)]
const BATCH_ROWS: usize = 2;
#[cfg(not(test))]
const BATCH_ROWS: usize = 500;

/// One buffered `term` row, with spans into the bank.
#[allow(clippy::type_complexity)]
type TermBatchRow = (Span, Option<Span>, Span, Span, Option<i64>, i64, i64);

const DDL: &str = "
CREATE TABLE dict (
    dict_id  INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entry (
    entry_id INTEGER PRIMARY KEY,
    dict_id  INTEGER NOT NULL REFERENCES dict(dict_id),
    -- The dictionary's raw structured-content glossary, verbatim. Parsed per
    -- hover into a `GlossDoc` behind a parsed-tree cache, so a parser or
    -- renderer fix ships as a patch and never as a rebuild
    -- (docs/research/hover-parse-cost.md).
    glossary TEXT NOT NULL
);

CREATE TABLE term (              -- the hot index; ~25 point queries per hover
    surface  TEXT NOT NULL,      -- scan key (kana or kanji surface form)
    written  TEXT,               -- kanji headword; NULL if the headword is kana-only
    reading  TEXT,
    pos      TEXT NOT NULL DEFAULT '',
    freq     INTEGER,            -- rank; lower = more common; NULL = unranked
    entry_id INTEGER NOT NULL REFERENCES entry(entry_id),
    dict_id  INTEGER NOT NULL REFERENCES dict(dict_id)  -- denormalised from entry (same reason as pos): grouping and dict-priority ranking cost no join on the hot path. Appended last so existing positional indices are unaffected.
);

CREATE TABLE meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

CREATE TABLE dict_style (        -- one row per dictionary that ships a styles.css
    dict_id INTEGER PRIMARY KEY REFERENCES dict(dict_id),
    -- The archive's own `styles.css`, verbatim, exactly as `entry.glossary`
    -- keeps its tree verbatim and for the same reason: the matcher compiles
    -- it once per process on first use, so a matcher fix ships as a patch
    -- and never as a rebuild. 14 of 72 corpus dictionaries ship one, 174 KB
    -- between them, which is not worth caching in a compiled form.
    --
    -- Its own table rather than a column on `dict`, so the dictionary list's
    -- own query never pages a 37 KB text through.
    css     TEXT NOT NULL
);

CREATE TABLE media_blob (        -- content-addressed asset bytes
    blob_id INTEGER PRIMARY KEY,
    -- SHA-256 of `bytes`, raw. The dedup key, and a database invariant
    -- rather than a build-time promise: 字通 averages more than four image
    -- nodes per term row over a few thousand distinct gaiji, so the same
    -- bytes arrive at many paths and sharing them is load-bearing.
    hash    BLOB NOT NULL UNIQUE,
    bytes   BLOB NOT NULL
);

CREATE TABLE media (             -- one row per referenced (dictionary, path)
    dict_id INTEGER NOT NULL REFERENCES dict(dict_id),
    path    TEXT NOT NULL,       -- the image node's own `path`, verbatim
    blob_id INTEGER NOT NULL REFERENCES media_blob(blob_id),
    format  TEXT NOT NULL,       -- 'png' | 'jpeg' | 'gif' | 'svg' | 'avif'
    -- The intrinsic size, in CSS pixels, read out of the container header
    -- at extraction time. 99 807 census image nodes declare neither `width`
    -- nor `height`, so without these three columns sizing an image would
    -- mean decoding it inside the measurement seam. `aspect` is
    -- `width / height`, carried rather than derived because the common
    -- `height: 1em` node is sized by multiplying.
    width   REAL NOT NULL,
    height  REAL NOT NULL,
    aspect  REAL NOT NULL,
    -- WITHOUT ROWID: the primary-key index *is* the table, so a
    -- (dict_id, path) probe is one B-tree descent with no rowid
    -- indirection, and a size probe never touches a blob page. The blobs
    -- live in their own table for exactly that reason.
    PRIMARY KEY (dict_id, path)
) WITHOUT ROWID;

CREATE TABLE reported_freq (     -- one dictionary's own claim, per headword
    dict_id INTEGER NOT NULL REFERENCES dict(dict_id),
    -- The headword the frequency archive named, and the reading it scoped
    -- the claim to, which is `FreqTable`'s key spelled out: term plus
    -- optional reading, so `lookup_freq`'s reading-scoped-then-agnostic rule
    -- reads back off these rows exactly as it reads off the table they came
    -- from. NULL reading = ranked whatever the reading.
    term    TEXT NOT NULL,
    reading TEXT,
    rank    INTEGER NOT NULL     -- lower = more common
    -- Indexed on `term` alone (see `INDEXES`), and on nothing else. The
    -- reindex and a removal both read one dictionary's claims whole and want
    -- a scan; the only point query is the popup's, which asks what the
    -- enabled dictionaries said about one headword and gets a handful of
    -- rows back. `term.freq` is still the reduced Frequency rank
    -- denormalised onto the hot row, so nothing on the term path comes here
    -- at all (ARCHITECTURE.md#dictionary-and-lookup).
);

CREATE TABLE pitch (             -- one dictionary's Pitch pattern, per reading
    dict_id  INTEGER NOT NULL REFERENCES dict(dict_id),
    -- The headword the pitch archive named and the reading it gave the
    -- accent for. `term` is `COALESCE(term.written, term.surface)` - the
    -- kanji headword, or the kana one where there is no kanji - which is
    -- how `term` above is keyed and the same expression `reported_freq` is
    -- probed with, so a card that has a headword and a reading in hand asks
    -- for its accents with no join. Both are NOT NULL because Yomitan skips
    -- a pitch payload whose reading is not the headword's: an accent with
    -- no reading applies to nothing.
    term     TEXT NOT NULL,
    reading  TEXT NOT NULL,
    -- Where the pitch falls, in the two forms the schema permits, exactly
    -- one per row. `downstep` is the 1-based count of moras before the fall
    -- with 0 meaning heiban, which is what all 511 488 censused accents
    -- are; `pattern` is the `^[HL]+$` level-per-mora form, which the schema
    -- permits and neither corpus uses. Two columns rather than one because
    -- the two forms share no indexing origin and the string can say things
    -- no integer can - several falls, or a word that neither falls nor
    -- starts low.
    downstep INTEGER,
    pattern  TEXT,
    -- The moras this accent marks nasal and devoiced, as JSON arrays of
    -- 1-based mora indices, and the accent's own tags. Stored and not
    -- drawn: mark drawing needs them, 25.8% of NHK's rows carry one, and
    -- dropping them here is exactly what would have cost a second schema
    -- bump.
    nasal    TEXT NOT NULL,
    devoice  TEXT NOT NULL,
    tags     TEXT NOT NULL
    -- Indexed on (term, reading) (see `INDEXES`) and on nothing else: the
    -- only read is one probe per shown card, and both columns are always
    -- given. Nothing on the term path comes here at all - pitch is per
    -- reading, so it cannot ride on a `term` row and must not widen one
    -- (ARCHITECTURE.md#dictionary-and-lookup).
);
";

/// Tables that `dict_id` references, in child-to-parent order.
/// [`crate::dict::edit::remove_dictionary`] walks this list.
///
/// Keep this list beside [`DDL`]. A prior change added `dict_style` to `DDL`
/// but omitted it here. Empty fixtures hid the defect because they had no
/// `styles.css` and therefore no `dict_style` rows. The removal then
/// succeeded and left that table's rows.
///
/// This list omits `dict` on purpose. The removal deletes its parent row *last*
/// and handles that row separately.
///
/// The order follows the foreign keys. `term` references `entry`, and each
/// table references `dict`, so the removal deletes child rows first. The
/// removal compares the *membership* of this list with [`dict_keyed_tables`].
/// An unknown table then stops removal and prevents orphan rows.
pub const DICT_KEYED: [&str; 6] =
    ["term", "entry", "media", "dict_style", "reported_freq", "pitch"];

/// Every table in this database with a `dict_id` column.
///
/// This function reads the live schema. It must find tables that this code
/// does not know.
/// `dict` belongs in the result because its primary key is `dict_id`.
/// The caller keeps `dict` for the final delete.
pub fn dict_keyed_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table'
               AND EXISTS (SELECT 1 FROM pragma_table_info(sqlite_master.name)
                           WHERE name = 'dict_id')
             ORDER BY name",
        )
        .context("preparing the dict_id-keyed table query")?;
    let found = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .context("listing the dict_id-keyed tables")?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("reading the dict_id-keyed tables")?;
    Ok(found)
}

const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_term_surface ON term(surface);
CREATE INDEX IF NOT EXISTS idx_term_entry_id ON term(entry_id);
CREATE INDEX IF NOT EXISTS idx_reported_freq_term ON reported_freq(term);
CREATE INDEX IF NOT EXISTS idx_pitch_term_reading ON pitch(term, reading);
";

/// Counts of rows written by a build.
pub struct BuildCounts {
    pub entries: i64,
    pub terms: i64,
    pub media: MediaCounts,
    pub styles: StyleCounts,
}

/// Counts for one build's Dictionary stylesheets.
///
/// The build stores stylesheet text and compiles it once to collect these counts.
/// It does not store compiled data. The matcher compiles the text on first use.
/// `dropped` counts rules whose selectors the current grammar cannot read.
/// `tools/dict-census` compares this count with the live grammar.
///
/// `declarations` and `unmapped` measure a separate property gap.
/// A rule can pass selector grammar but still use properties that this renderer lacks.
/// `display: grid` is common in the corpus and shows this property gap.
#[derive(Clone, Copy, Default, Debug)]
pub struct StyleCounts {
    /// Dictionaries with a `styles.css` file.
    pub sheets: usize,
    /// CSS bytes that the build stores.
    pub bytes: usize,
    /// Rules that the build keeps in its rule table.
    pub kept: usize,
    /// Rules that the build drops: an unsupported selector or an at-rule body.
    pub dropped: usize,
    /// Selectors that the build compiles after it expands selector lists and
    /// `&`-nested selectors.
    pub selectors: usize,
    /// Declarations that the build compiles onto a
    /// [`crate::dict::gloss::StyleKey`]. The build expands each `margin` and
    /// `padding` shorthand into four longhands. This count includes kept rules
    /// only. A rule with no mapped declaration counts neither here nor in
    /// `dropped`. It has a property gap, not a grammar gap.
    pub declarations: usize,
    /// Declarations that this build cannot express.
    /// This includes a property outside `sheet::css_key` and a `var()` value
    /// that names a custom property from Yomitan's popup chrome.
    /// This renderer has no equivalent for that chrome.
    pub unmapped: usize,
    /// Stylesheets that the scanner cannot read without errors.
    /// The build still compiles each rule that the scanner recovers.
    /// This field counts stylesheets, not lost rules.
    pub malformed: usize,
}

impl StyleCounts {
    fn add(&mut self, other: StyleCounts) {
        self.sheets += other.sheets;
        self.bytes += other.bytes;
        self.kept += other.kept;
        self.dropped += other.dropped;
        self.selectors += other.selectors;
        self.declarations += other.declarations;
        self.unmapped += other.unmapped;
        self.malformed += other.malformed;
    }

    /// The one-line diagnostic, in the progress stream's own shape.
    fn line(&self, what: &str) -> String {
        let kib = self.bytes.div_ceil(1024);
        format!(
            "styles    {what}: {} sheets, {kib} KiB, {} rules kept, \
             {} dropped, {} selectors, {} declarations ({} unmapped); \
             {} malformed",
            self.sheets,
            self.kept,
            self.dropped,
            self.selectors,
            self.declarations,
            self.unmapped,
            self.malformed,
        )
    }
}

/// Counts from media that the build extracts from one corpus or one archive.
///
/// Each field supports a required diagnostic. The fields count named assets,
/// stored assets, database bytes, and unresolved assets.
/// The two failure counts stay separate because they have different causes.
/// `missing` means an archive lacks referenced bytes.
/// `unreadable` means the archive has a corrupt file.
#[derive(Clone, Copy, Default, Debug)]
pub struct MediaCounts {
    /// Distinct asset paths that kept rows reference.
    pub referenced: usize,
    /// Media rows written by the build.
    pub stored: usize,
    /// New blobs that this run adds after it maps equal bytes to one blob.
    pub blobs: usize,
    /// Bytes that new blobs add to the database.
    pub bytes: u64,
    /// Assets that image nodes reference but archives do not supply.
    pub missing: usize,
    /// Files that archives supply but the build cannot size.
    pub unreadable: usize,
}

impl MediaCounts {
    fn add(&mut self, other: MediaCounts) {
        self.referenced += other.referenced;
        self.stored += other.stored;
        self.blobs += other.blobs;
        self.bytes += other.bytes;
        self.missing += other.missing;
        self.unreadable += other.unreadable;
    }

    /// The one-line diagnostic, in the progress stream's own shape.
    fn line(&self, what: &str) -> String {
        let kib = self.bytes.div_ceil(1024);
        format!(
            "media     {what}: {} of {} assets in {} blobs, {kib} KiB\
             ; {} missing, {} unreadable",
            self.stored, self.referenced, self.blobs, self.missing, self.unreadable,
        )
    }
}

/// Separators that `json.dumps` writes.
#[derive(Default)]
struct PySpaced;

impl serde_json::ser::Formatter for PySpaced {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

/// Encodes `value` with the separators that `json.dumps` writes.
fn to_py_json<T: Serialize>(value: &T) -> Result<String> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PySpaced);
    value.serialize(&mut ser)?;
    String::from_utf8(buf).context("json output was not utf-8")
}

/// Builds chibipop.sqlite.
pub fn build(
    terms: &[PathBuf],
    freqs: &[PathBuf],
    out: &Path,
    on_progress: &dyn Fn(&str),
) -> Result<BuildCounts> {
    if terms.is_empty() {
        anyhow::bail!("no term archives to build from");
    }

    // A fresh executable has no data/ directory yet.
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Never destroy `out` when the build fails.
    let tmp = suffixed(out, ".building");
    if tmp.exists() {
        std::fs::remove_file(&tmp).with_context(|| format!("removing {}", tmp.display()))?;
    }

    let counts = build_into(terms, freqs, &tmp, on_progress).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    promote(&tmp, out, on_progress).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(counts)
}

/// Returns `out` with `suffix` appended to the complete file name.
///
/// This function appends the suffix and keeps the extension. SQLite derives
/// `chibipop.sqlite-wal`. `chibipop.sqlite.building` stays beside the file
/// that the build promotes.
fn suffixed(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Replaces the live database with a checked build.
///
/// The order of these steps protects the live database:
///
/// 1. **Check the built file.** [`build_into`] uses `synchronous = OFF` for
///    the bulk load. The file is temporary, so lower durability is safe. `quick_check`
///    is the last check that can reject a bad build before promotion.
/// 2. **Remove the old `-wal` and `-shm` files.** SQLite keys these sidecars
///    to the database *file name*, not its inode. A rename replaces only the
///    main file. If the old sidecars remain, the next reader can recover old
///    pages into the new database. The checkpoint before removal preserves them.
/// 3. **Rename.** This is the only instant when the live database changes.
///
/// Each earlier failure leaves a readable database:
///
/// - A failure in step 1 leaves `out` unchanged. The next build removes the
///   orphaned `.building` file.
/// - A failure in step 2 leaves the old `out` complete. The checkpoint copied
///   log pages before removal. An open reader stays safe because unlink removes
///   a name, not an open file.
/// - Step 3 uses atomic rename. Both files are complete and have no sidecars.
///
/// Do not rename before you remove the sidecars. That order lets old log pages
/// enter the new database.
fn promote(tmp: &Path, out: &Path, on_progress: &dyn Fn(&str)) -> Result<()> {
    verify_built(tmp, on_progress)?;
    drain_wal(out);
    drop_sidecars(out)?;
    std::fs::rename(tmp, out)
        .with_context(|| format!("replacing {} with {}", out.display(), tmp.display()))
}

/// Checks a completed build before promotion.
///
/// The checkpoint makes `quick_check` read pages that a fresh reader reads.
/// It also leaves the file with no WAL frames. The second step calls `fsync`
/// because `synchronous = OFF` does not flush bytes from the page cache.
/// `quick_check` reads *through* that cache, so it cannot prove that bytes reached
/// disk. Rename has value only when it publishes a durable file.
fn verify_built(built: &Path, on_progress: &dyn Fn(&str)) -> Result<()> {
    on_progress("building  checking the new database");
    let conn = Connection::open(built)
        .with_context(|| format!("reopening {} to check it", built.display()))?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .with_context(|| format!("checkpointing {}", built.display()))?;
    // Use `quick_check`, not `integrity_check`. It performs the page structure
    // check but skips the index-to-table check. That check takes minutes on a
    // multi-gigabyte corpus.
    let verdict: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .with_context(|| format!("checking {}", built.display()))?;
    if verdict != "ok" {
        anyhow::bail!(
            "the dictionary this build wrote did not check out ({verdict}); \
             your existing dictionary has been left alone"
        );
    }
    drop(conn);
    // Use `write(true)`, not `File::open`. Windows requires write access for
    // `FlushFileBuffers`. A read-only handle returns `ERROR_ACCESS_DENIED` and
    // makes every Windows build fail with "Access is denied. (os error 5)".
    File::options()
        .write(true)
        .open(built)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("flushing {} to disk", built.display()))?;
    Ok(())
}

/// Copies WAL pages into the live database before the build removes the log.
///
/// This function tries to finish the checkpoint. A reader in a transaction can reject a
/// `TRUNCATE` checkpoint. A file too damaged to open holds no usable pages.
/// Either case lets the build remove the old database.
/// This work protects step 2 of [`promote`]. After success, the old database
/// is complete by itself, so a crash before rename leaves no fragment.
fn drain_wal(out: &Path) {
    if !out.exists() {
        return;
    }
    if let Ok(conn) = Connection::open(out) {
        // Do not wait. The daemon serves lookups during a rebuild, and a reader
        // often holds a snapshot. A five-second busy timeout can delay every rebuild.
        // A failed checkpoint leaves the database unchanged. `PASSIVE` copies frames
        // that readers no longer need and does not block. `TRUNCATE` then finishes
        // when readers sit between transactions.
        let _ = conn.execute_batch(
            "PRAGMA busy_timeout = 0;
             PRAGMA wal_checkpoint(PASSIVE);
             PRAGMA wal_checkpoint(TRUNCATE);",
        );
    }
}

/// Removes sidecars that the previous database left under this file name.
///
/// This function returns an error and reports failure. A later reader
/// can recover any sidecar that remains into the new file. If promotion fails,
/// the user keeps a sound Dictionary instead of a malformed one.
/// The error also covers a platform that cannot unlink an open file.
fn drop_sidecars(out: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let path = suffixed(out, suffix);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "removing {}, which belongs to the dictionary being replaced and \
                         would be recovered into the new one",
                        path.display()
                    )
                })
            }
        }
    }
    Ok(())
}

fn build_into(
    terms: &[PathBuf],
    freqs: &[PathBuf],
    out: &Path,
    on_progress: &dyn Fn(&str),
) -> Result<BuildCounts> {
    let sources = load_freqs(freqs)?;
    // A fresh build enables every frequency Dictionary in library order. It has
    // no disabled state or user order, so it applies the default Ranking strategy.
    // A user who changes the strategy runs Reindex. Reindex handles that change
    // and costs seconds, while this function costs minutes.
    let strategy = RankingStrategy::default();
    let names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
    let tables: Vec<FreqTable> = sources.into_iter().map(|s| s.table).collect();
    let ranks = frequency::reduce(&tables, strategy);

    let mut conn = Connection::open(out).with_context(|| format!("creating {}", out.display()))?;
    // These settings serve only the bulk load. The file is temporary, and
    // `promote` checks it before any reader uses it. Extra durability adds no
    // protection beyond `quick_check` here.
    //
    // Use `journal_mode = MEMORY`, not `WAL`. A WAL writes each page twice during
    // a half-gigabyte load: once to the log and once during checkpoint.
    // The load has no concurrent reader. An empty file gives a rollback journal
    // almost no original pages to save, and `MEMORY` saves none.
    // The code below changes the finished file to WAL because
    // `edit::add_dictionary` writes that file while readers use it.
    //
    // Add no other settings. A larger `cache_size` and `temp_store = MEMORY` add
    // peak memory, but jitendex import does not become faster. An append-only load
    // does not read rows again. The index sorter writes to a file that the OS
    // caches. A 128 MiB page cache added 150 MiB RSS and saved 0.08s.
    conn.execute_batch(
        "PRAGMA page_size = 8192;
         PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;",
    )?;
    create_schema(&conn)?;

    let mut entries: i64 = 0;
    let mut term_rows: i64 = 0;
    let mut media = MediaCounts::default();
    let mut styles = StyleCounts::default();
    let mut batches = Batches::new();

    let tx = conn.transaction()?;
    // Use the `dict_id` that the build assigns to each archive. The pitch pass
    // stores each archive's accents under that row, not under a name.
    // `dict.name` is a title, and two editions can share one title.
    let mut read: Vec<(i64, &Path)> = Vec::with_capacity(terms.len() + freqs.len());
    for (i, archive) in terms.iter().enumerate() {
        let slot =
            Slot { dict_id: i as i64 + 1, priority: i as i64, first_entry_id: entries + 1 };
        let one = insert_archive(&tx, archive, &slot, &ranks, &mut batches, on_progress)?;
        entries += one.entries;
        term_rows += one.terms;
        media.add(one.media);
        styles.add(one.styles);
        read.push((slot.dict_id, archive.as_path()));
    }

    // Store rows from the frequency archives. One archive can provide frequency
    // data and terms, so it appears in both lists but remains one Dictionary.
    // Store its claims under the current `dict_id`, not under a second row.
    // A role set lets one archive appear twice here
    // (ARCHITECTURE.md#dictionary-and-lookup).
    // A frequency archive absent from `terms` gets a new row after term
    // Dictionaries. This keeps each term archive's `dict_id` equal to its
    // position in `terms`.
    // The build stores the reduced rank beside each claim, so a reader knows
    // which Dictionaries produced each `term` value.
    let mut order = Vec::with_capacity(tables.len());
    let mut appended: i64 = 0;
    for (i, (name, table)) in names.iter().zip(&tables).enumerate() {
        let dict_id = match read.iter().find(|(_, path)| *path == freqs[i].as_path()) {
            Some(&(already, _)) => already,
            None => {
                appended += 1;
                let dict_id = terms.len() as i64 + appended;
                tx.execute(
                    "INSERT INTO dict (dict_id, name, priority) VALUES (?1, ?2, ?3)",
                    params![dict_id, name, dict_id - 1],
                )?;
                read.push((dict_id, freqs[i].as_path()));
                dict_id
            }
        };
        reindex::store_reported(&tx, dict_id, table)?;
        order.push(dict_id);
    }
    reindex::record(&tx, &reindex::Reduction { order, strategy })?;

    let accents = store_archive_pitch(&tx, &read, on_progress)?;

    write_meta(&tx, terms, freqs)?;
    on_progress("building  creating index");
    ensure_indexes(&tx)?;
    tx.commit()?;
    // Readers open the promoted database in this mode. See the pragma block
    // above.
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    // `analysis_limit` caps the index rows that each statistic samples. The
    // planner uses these statistics to choose indexes, such as `surface`.
    // A 400-row sample gives the planner the same choice as a full scan of 660 000
    // term rows, but costs much less time.
    conn.execute_batch("PRAGMA analysis_limit = 400; ANALYZE;")?;

    if media.referenced > 0 {
        on_progress(&media.line("all dictionaries"));
    }
    if styles.sheets > 0 {
        on_progress(&styles.line("all dictionaries"));
    }
    if accents > 0 {
        on_progress(&format!("pitch     all dictionaries: {accents} accents"));
    }
    Ok(BuildCounts { entries, terms: term_rows, media, styles })
}

/// Stores every archive's Pitch patterns under the Dictionary that supplied them.
///
/// This function visits every archive that the build read. The
/// `term_meta_bank_` rows define the Pitch role. The archive input list does
/// not define that role. A pitch-only archive, a term archive, and a frequency
/// archive can each supply Pitch data. Each uses its current `dict` row.
///
/// The enabled Pitch list controls reader order. Config owns that list, so
/// the build does not read it (ARCHITECTURE.md#dictionary-and-lookup).
/// The same split applies to `reported_freq`: the build stores every claim,
/// and the enabled list controls those claims.
///
/// Returns the number of stored accents for the progress line.
fn store_archive_pitch(
    tx: &rusqlite::Transaction,
    read: &[(i64, &Path)],
    on_progress: &dyn Fn(&str),
) -> Result<usize> {
    let mut total = 0;
    for &(dict_id, archive) in read {
        // Load Pitch data before the title. An archive without a term-meta
        // bank gets its title from the central directory. The title path then reads
        // `index.json` again but finds no result.
        let table = pitch::load_pitch(archive)
            .with_context(|| format!("reading the pitch of {}", archive.display()))?;
        if table.is_empty() {
            continue;
        }
        let stored = store_pitch(tx, dict_id, &table)?;
        total += stored;
        on_progress(&format!(
            "pitch     [{}] {stored} accents over {} readings",
            dict_title(archive)?,
            table.len(),
        ));
    }
    Ok(total)
}

/// Stores one Dictionary's Pitch patterns, with one row per accent.
///
/// Store one row per accent, not one row per reading with a packed list.
/// A card header draws each reading's accents from separate rows.
/// A reader that splits a packed column cannot use the index to find accents.
pub(crate) fn store_pitch(
    tx: &rusqlite::Transaction,
    dict_id: i64,
    table: &pitch::PitchTable,
) -> Result<usize> {
    let mut insert = tx
        .prepare(
            "INSERT INTO pitch \
             (dict_id, term, reading, downstep, pattern, nasal, devoice, tags) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .context("preparing the pitch insert")?;
    let mut rows = 0;
    for ((term, reading), accents) in table {
        for accent in accents {
            let (downstep, pattern) = match &accent.position {
                pitch::Position::Downstep(fall) => (Some(i64::from(*fall)), None),
                pitch::Position::Pattern(levels) => (None, Some(levels.as_str())),
            };
            insert
                .execute(params![
                    dict_id,
                    term,
                    reading,
                    downstep,
                    pattern,
                    to_json_list(&accent.nasal)?,
                    to_json_list(&accent.devoice)?,
                    to_json_list(&accent.tags)?,
                ])
                .with_context(|| format!("storing the pitch of {term} / {reading}"))?;
            rows += 1;
        }
    }
    Ok(rows)
}

/// Encodes one accent's mora markers or tags as the JSON array in its column.
///
/// Use JSON, not a separated string. An author's tag can contain any
/// separator. One format for all three columns gives the reader and writer
/// one implementation each.
fn to_json_list<T: Serialize>(list: &[T]) -> Result<String> {
    serde_json::to_string(list).context("encoding an accent's mora list")
}

/// Loads each frequency archive's claims in archive order.
///
/// This function groups rows by archive and does not merge archives.
/// `merge_freq_row` keeps the lowest rank within one archive.
/// A Ranking strategy chooses how to compare claims across archives.
/// Earlier code ended with `table.extend(one)`. Then the last archive won
/// every key that it named, and the build gave no diagnostic.
pub fn load_freqs(freqs: &[PathBuf]) -> Result<Vec<FreqSource>> {
    let mut sources = Vec::with_capacity(freqs.len());
    for fa in freqs {
        let mut table = FreqTable::new();
        for_each_meta_row(fa, |row| {
            merge_freq_row(&mut table, &row);
            Ok(())
        })?;
        sources.push(FreqSource { name: dict_title(fa)?, table });
    }
    Ok(sources)
}

/// Where one archive lands.
pub(crate) struct Slot {
    pub(crate) dict_id: i64,
    pub(crate) priority: i64,
    pub(crate) first_entry_id: i64,
}

/// What one archive contributed.
pub(crate) struct Loaded {
    pub(crate) name: String,
    pub(crate) entries: i64,
    pub(crate) terms: i64,
    pub(crate) media: MediaCounts,
    pub(crate) styles: StyleCounts,
}

/// A `(start, end)` byte slice in a [`PreparedBank`] buffer.
///
/// A pair of `u32` values avoids one `String` per field. A bank holds tens
/// of thousands of rows, and each `String` allocates four times per row.
/// The binder copies each span once. The buffer holds at most one bank's text.
/// [`crate::dict::archive`] sets that limit at `MAX_BANK`, 256 MiB.
/// The bank already stores every byte that the build pushes into the buffer,
/// so `u32` cannot overflow.
type Span = (u32, u32);

/// One kept term-bank row, ready to bind.
#[derive(Clone, Copy)]
struct PreparedRow {
    glossary: Span,
    written: Span,
    reading: Span,
    rules: Span,
    freq: Option<i64>,
    /// The headword is kana-only, so it needs one `term` row and not two.
    same: bool,
}

/// Rows that one bank keeps, plus the asset paths that those rows name.
///
/// One growable buffer holds all data from a bank. The parser thread then
/// gives the writer thread three allocations, not a hundred thousand.
struct PreparedBank {
    text: String,
    rows: Vec<PreparedRow>,
    assets: BTreeSet<String>,
}

/// Maximum number of threads that parse banks during an import.
///
/// This limit protects memory. Each thread holds its source bank and its
/// prepared bank, so the limit bounds peak memory at about
/// `2 * MAX_IMPORT_THREADS` bank-sized buffers.
/// The project measured eight threads past the useful point on every corpus.
/// At about four threads, SQLite inserts in the writer thread become the limit.
const MAX_IMPORT_THREADS: usize = 8;

fn worker_count(banks: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    banks.min(cores).clamp(1, MAX_IMPORT_THREADS)
}

/// Buffers that archives share.
pub(crate) struct Batches {
    /// Row buffers reused by every bank. They hold spans into the current bank,
    /// so they hold no data between banks.
    entries: Vec<(i64, i64, Span)>,
    terms: Vec<TermBatchRow>,
    /// SQL for full-batch `INSERT`s. The code builds each statement once.
    /// A 500-row insert names 3 500 placeholders. Most flushes use a full batch.
    /// A short batch covers a bank's final rows or one extra row from a two-row entry.
    entry_sql: String,
    term_sql: String,
    /// Content hashes mapped to `media_blob.blob_id` across one build.
    /// The build stores shared asset bytes once. A second Dictionary then needs one
    /// `SELECT`, not one `SELECT` per path.
    blobs: HashMap<[u8; 32], i64>,
}

impl Batches {
    pub(crate) fn new() -> Batches {
        Batches {
            entries: Vec::with_capacity(BATCH_ROWS),
            terms: Vec::with_capacity(BATCH_ROWS + 1),
            entry_sql: insert_sql(ENTRY_INSERT, 3, BATCH_ROWS),
            term_sql: insert_sql(TERM_INSERT, 7, BATCH_ROWS),
            blobs: HashMap::new(),
        }
    }
}

/// Inserts one archive into one `Slot`.
///
/// `ranks` holds the reduced Frequency rank for each headword.
/// [`frequency::reduce`] combines the enabled frequency Dictionaries with the
/// selected Ranking strategy. The build writes that result to `term.freq`.
/// A reader never applies the strategy to that column.
pub(crate) fn insert_archive(
    tx: &rusqlite::Transaction,
    archive: &Path,
    slot: &Slot,
    ranks: &FreqTable,
    batches: &mut Batches,
    on_progress: &dyn Fn(&str),
) -> Result<Loaded> {
    let dict_id = slot.dict_id;
    let name = dict_title(archive)?;
    tx.execute(
        "INSERT INTO dict (dict_id, name, priority) VALUES (?1, ?2, ?3)",
        params![dict_id, name, slot.priority],
    )?;
    let styles = insert_style(tx, archive, dict_id, &name, on_progress)?;

    let mut entry_id = slot.first_entry_id - 1;
    let mut term_rows: i64 = 0;
    let mut assets: BTreeSet<String> = BTreeSet::new();

    for_each_prepared_bank(archive, ranks, |bank| {
        write_bank(tx, &bank, dict_id, &mut entry_id, &mut term_rows, batches, on_progress)?;
        if assets.is_empty() {
            assets = bank.assets;
        } else {
            assets.extend(bank.assets);
        }
        Ok(())
    })?;

    let media = insert_media(tx, archive, dict_id, &name, &assets, batches, on_progress)?;
    Ok(Loaded {
        name,
        entries: entry_id + 1 - slot.first_entry_id,
        terms: term_rows,
        media,
        styles,
    })
}

/// Writes one prepared bank to `entry` and `term`.
///
/// This function writes a complete bank before it reads the next one.
/// Row buffers then hold spans only into `bank.text`.
fn write_bank(
    tx: &rusqlite::Transaction,
    bank: &PreparedBank,
    dict_id: i64,
    entry_id: &mut i64,
    term_rows: &mut i64,
    batches: &mut Batches,
    on_progress: &dyn Fn(&str),
) -> Result<()> {
    for row in &bank.rows {
        *entry_id += 1;
        if *entry_id % 5000 == 0 {
            on_progress(&format!("progress  {entry_id} / ?"));
        }

        batches.entries.push((*entry_id, dict_id, row.glossary));
        batches.terms.push((
            row.reading,
            if row.same { None } else { Some(row.written) },
            row.reading,
            row.rules,
            row.freq,
            *entry_id,
            dict_id,
        ));
        *term_rows += 1;
        if !row.same {
            batches.terms.push((
                row.written,
                Some(row.written),
                row.reading,
                row.rules,
                row.freq,
                *entry_id,
                dict_id,
            ));
            *term_rows += 1;
        }

        if batches.entries.len() >= BATCH_ROWS || batches.terms.len() >= BATCH_ROWS {
            // Flush `entry` rows first. Each `term` row names its related `entry` row.
            flush_entries(tx, &bank.text, batches)?;
            flush_terms(tx, &bank.text, batches)?;
        }
    }
    flush_entries(tx, &bank.text, batches)?;
    flush_terms(tx, &bank.text, batches)
}

/// Processes every term bank in one archive.
/// Worker threads prepare banks apart from the writer thread.
/// This function passes prepared banks to the writer in archive order.
///
/// The order protects stable `entry_id` values. The build assigns an
/// `entry_id` as each bank arrives. Completion order changes the database
/// between runs. Workers can finish in any order, so this function restores
/// archive order. The reorder buffer holds at most one bank of extra latency
/// for each out-of-order result.
///
/// A bank is the work unit because it contains complete JSON. The build
/// parses one bank, parses its glossaries, and tests for renderable text.
/// That work uses about four fifths of import CPU and shares no state between
/// banks. SQLite writes alone run in series.
fn for_each_prepared_bank(
    archive: &Path,
    ranks: &FreqTable,
    mut on_bank: impl FnMut(PreparedBank) -> Result<()>,
) -> Result<()> {
    let mut banks = TermBanks::open(archive)?;
    let count = banks.len();
    let threads = worker_count(count);
    if count == 0 {
        return Ok(());
    }
    if threads == 1 {
        for i in 0..count {
            let text = banks.read(i)?;
            on_bank(prepare_bank(&text, banks.name(i), ranks)?)?;
        }
        return Ok(());
    }
    drop(banks);

    let next = AtomicUsize::new(0);
    // Use a rendezvous channel. An early worker blocks and does not buffer banks
    // that no caller needs. The channel and the reorder buffer bound memory.
    let (send, recv) = sync_channel::<(usize, Result<PreparedBank>)>(0);

    std::thread::scope(|scope| -> Result<()> {
        for _ in 0..threads {
            let send = send.clone();
            let next = &next;
            scope.spawn(move || {
                let mut banks = match TermBanks::open(archive) {
                    Ok(banks) => banks,
                    Err(why) => {
                        let _ = send.send((count, Err(why)));
                        return;
                    }
                };
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        return;
                    }
                    let made = banks
                        .read(i)
                        .and_then(|text| prepare_bank(&text, banks.name(i), ranks));
                    // A closed channel means that the writer stopped.
                    if send.send((i, made)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(send);

        let mut held: BTreeMap<usize, PreparedBank> = BTreeMap::new();
        let mut want = 0usize;
        while want < count {
            let (i, made) = recv
                .recv()
                .context("a dictionary import thread stopped without a result")?;
            held.insert(i, made?);
            while let Some(bank) = held.remove(&want) {
                on_bank(bank)?;
                want += 1;
            }
        }
        Ok(())
    })
}

/// Converts one bank's text into rows for the writer.
///
/// Import work per row occurs here: row parse, glossary parse, and the
/// empty-content test. This function does not access the database.
fn prepare_bank(text: &str, name: &str, ranks: &FreqTable) -> Result<PreparedBank> {
    let mut bank =
        PreparedBank { text: String::with_capacity(text.len()), rows: Vec::new(), assets: BTreeSet::new() };
    let mut glossary = String::new();

    for_each_row(text, name, |t| {
        // Minify first, then parse the stored text. A hover reads this same
        // record, so the render test matches the stored record.
        glossary.clear();
        minify_json(t.glossary, &mut glossary);
        // Reject an image-only or whitespace-only glossary. The earlier builder used
        // the same rule. This keeps a gaiji-only `term` row out of the term index.
        let doc = GlossDoc::parse(&glossary);
        if !renders_text(&doc) {
            return Ok(());
        }
        // Collect assets from the parsed document. The empty-content test already
        // parsed it, so this code avoids a second raw-JSON scan. A row without text
        // is not an entry, and a hover cannot reach its assets.
        collect_assets(&doc, &mut bank.assets);

        let reading: &str = if t.reading.is_empty() { &t.term } else { &t.reading };
        let freq = lookup_freq(ranks, &t.term, Some(reading));
        let same = t.term == reading;

        let glossary = push_span(&mut bank.text, &glossary);
        let written = push_span(&mut bank.text, &t.term);
        let reading = if same { written } else { push_span(&mut bank.text, reading) };
        let rules = push_span(&mut bank.text, &t.rules);
        bank.rows.push(PreparedRow { glossary, written, reading, rules, freq, same });
        Ok(())
    })?;

    Ok(bank)
}

/// Appends `s` and returns where it landed.
fn push_span(buf: &mut String, s: &str) -> Span {
    let start = buf.len() as u32;
    buf.push_str(s);
    (start, buf.len() as u32)
}

/// A span's text.
fn slice(text: &str, span: Span) -> &str {
    &text[span.0 as usize..span.1 as usize]
}

/// The archive glossary without whitespace outside JSON strings.
///
/// The build stores all other bytes verbatim. The `entry.glossary` column
/// therefore keeps the original content bytes. The previous writer passed
/// every glossary through `serde_json::Value` and a serializer. That round
/// trip used two of every six seconds in a jitendex import and sorted object
/// keys without a diagnostic.
///
/// Remove whitespace because a pretty-printed jitendex bank is 9% larger than
/// its minified form. That space adds 9% to a half-gigabyte database, and no
/// reader uses it.
fn minify_json(json: &str, into: &mut String) {
    let b = json.as_bytes();
    let mut copied = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for i in 0..b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c.is_ascii_whitespace() {
            // Copy complete runs. Compact JSON contains almost all bytes, so one
            // `push_str` copies nearly the whole glossary.
            into.push_str(&json[copied..i]);
            copied = i + 1;
        }
    }
    into.push_str(&json[copied..]);
}

/// Stores one Dictionary's `styles.css` text and counts it once.
///
/// The build stores stylesheet text and does not compile it for the row.
/// The matcher compiles text on first use. A matcher fix needs a patch, like a parser
/// fix. The build discards the compiled result and fills only the build report.
/// `dropped` gives the census its grammar-gap count.
///
/// The build accepts a stylesheet read failure. An archive with an unreadable
/// stylesheet stores no CSS and the build continues. The Dictionary entries
/// matter more than its stylesheet.
fn insert_style(
    tx: &rusqlite::Transaction,
    archive: &Path,
    dict_id: i64,
    name: &str,
    on_progress: &dyn Fn(&str),
) -> Result<StyleCounts> {
    let Some(css) = read_styles_css(archive)? else { return Ok(StyleCounts::default()) };
    tx.execute(
        "INSERT INTO dict_style (dict_id, css) VALUES (?1, ?2)",
        params![dict_id, css],
    )
    .with_context(|| format!("storing the stylesheet of {name}"))?;
    let counts = crate::dict::sheet::Sheet::compile(&css).counts().clone();
    let one = StyleCounts {
        sheets: 1,
        bytes: css.len(),
        kept: counts.kept,
        dropped: counts.dropped(),
        selectors: counts.selectors,
        declarations: counts.declarations,
        unmapped: counts.dropped_declarations,
        malformed: usize::from(counts.error.is_some()),
    };
    on_progress(&one.line(name));
    if let Some(error) = counts.error {
        on_progress(&format!("styles    {name}: did not scan cleanly - {error}"));
    }
    Ok(one)
}

/// Collects every asset path named by image nodes in a [`GlossDoc`].
///
/// The function scans `all_nodes` in parse order instead of a tree walk.
/// An image node is a leaf, so tree shape adds no information.
/// It checks `Kind::Image`, not `Tag::Img`, because a `type: "image"` glossary
/// item can be an image without a tag.
fn collect_assets(doc: &GlossDoc, into: &mut BTreeSet<String>) {
    for (i, node) in doc.all_nodes().iter().enumerate() {
        if node.kind != Kind::Image {
            continue;
        }
        let path = doc.attr_of(i as NodeId, "path").and_then(|v| doc.scalar_str(v));
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            into.insert(path.to_string());
        }
    }
}

/// Extracts referenced assets from one archive into the Media store.
///
/// The contract covers absent assets and the `alt`-text ladder:
/// **a media row exists only when the store has the bytes and the intrinsic
/// size.**
/// An absent path or unreadable size creates no row and one diagnostic.
/// Neither case fails the build. Archives contain third-party bytes, so one
/// corrupt gaiji must not fail the complete rebuild.
fn insert_media(
    tx: &rusqlite::Transaction,
    archive: &Path,
    dict_id: i64,
    name: &str,
    assets: &BTreeSet<String>,
    batches: &mut Batches,
    on_progress: &dyn Fn(&str),
) -> Result<MediaCounts> {
    let mut counts = MediaCounts { referenced: assets.len(), ..MediaCounts::default() };
    if assets.is_empty() {
        return Ok(counts);
    }

    let mut refused: Vec<String> = Vec::new();
    let missing = for_each_media(archive, assets, |path, bytes| {
        let size = match media::probe(bytes) {
            Ok(size) => size,
            Err(why) => {
                counts.unreadable += 1;
                if refused.len() < DIAGNOSTIC_SAMPLE {
                    refused.push(format!("{path} ({why})"));
                }
                return Ok(());
            }
        };
        let blob_id = blob_id(tx, &mut batches.blobs, bytes, &mut counts)?;
        insert_media_row(tx, dict_id, path, blob_id, size)?;
        counts.stored += 1;
        Ok(())
    })?;
    counts.missing = missing.len();

    on_progress(&counts.line(name));
    for line in refused {
        on_progress(&format!("media     skipped {line}"));
    }
    for path in missing.iter().take(DIAGNOSTIC_SAMPLE) {
        on_progress(&format!("media     absent from the archive: {path}"));
    }
    Ok(counts)
}

/// Maximum number of names that one diagnostic lists.
///
/// Without this limit, a Dictionary with no assets can print one line per image
/// node. 字通 has 139 138 nodes. The count stays complete, and the names
/// provide a sample.
const DIAGNOSTIC_SAMPLE: usize = 10;

fn insert_media_row(
    tx: &rusqlite::Transaction,
    dict_id: i64,
    path: &str,
    blob_id: i64,
    size: Intrinsic,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO media (dict_id, path, blob_id, format, width, height, aspect) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        dict_id,
        path,
        blob_id,
        size.format.as_str(),
        f64::from(size.width),
        f64::from(size.height),
        f64::from(size.aspect),
    ])
    .with_context(|| format!("storing media {path} of dictionary {dict_id}"))?;
    Ok(())
}

/// Gets the `media_blob` row for these bytes. The first call inserts the blob.
///
/// Insert with `INSERT OR IGNORE` and then read. Do not read first and then
/// insert. `edit::add_dictionary` writes to a live database whose blob table
/// this build did not fill, so the row can already exist.
/// The in-memory map keeps one statement pair per distinct blob.
fn blob_id(
    tx: &rusqlite::Transaction,
    blobs: &mut HashMap<[u8; 32], i64>,
    bytes: &[u8],
    counts: &mut MediaCounts,
) -> Result<i64> {
    let hash = sha256(bytes);
    if let Some(&id) = blobs.get(&hash) {
        return Ok(id);
    }
    let inserted = tx
        .prepare_cached("INSERT OR IGNORE INTO media_blob (hash, bytes) VALUES (?1, ?2)")?
        .execute(params![&hash[..], bytes])
        .context("storing an asset's bytes")?;
    let id: i64 = tx
        .prepare_cached("SELECT blob_id FROM media_blob WHERE hash = ?1")?
        .query_row(params![&hash[..]], |r| r.get(0))
        .context("reading back an asset's blob id")?;
    if inserted == 1 {
        counts.blobs += 1;
        counts.bytes += bytes.len() as u64;
    }
    blobs.insert(hash, id);
    Ok(id)
}

/// SHA-256 of a slice already in memory.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize()
}

const ENTRY_INSERT: &str = "INSERT INTO entry (entry_id, dict_id, glossary) VALUES ";
const TERM_INSERT: &str =
    "INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id) VALUES ";

/// Returns `head` followed by `rows` tuples with `cols` numbered placeholders.
fn insert_sql(head: &str, cols: usize, rows: usize) -> String {
    use std::fmt::Write;
    let mut sql = String::with_capacity(head.len() + rows * cols * 7);
    sql.push_str(head);
    let mut n = 1;
    for row in 0..rows {
        sql.push_str(if row == 0 { "(" } else { ",(" });
        for col in 0..cols {
            let _ = write!(sql, "{}?{n}", if col == 0 { "" } else { "," });
            n += 1;
        }
        sql.push(')');
    }
    sql
}

/// Flushes buffered entry rows.
fn flush_entries(tx: &rusqlite::Transaction, text: &str, batches: &mut Batches) -> Result<()> {
    let batch = &batches.entries;
    if batch.is_empty() {
        return Ok(());
    }
    let owned;
    let sql = if batch.len() == BATCH_ROWS {
        &batches.entry_sql
    } else {
        owned = insert_sql(ENTRY_INSERT, 3, batch.len());
        &owned
    };
    let mut stmt = tx.prepare_cached(sql)?;
    let mut idx = 1;
    for row in batch.iter() {
        stmt.raw_bind_parameter(idx, row.0)?;
        stmt.raw_bind_parameter(idx + 1, row.1)?;
        stmt.raw_bind_parameter(idx + 2, slice(text, row.2))?;
        idx += 3;
    }
    stmt.raw_execute()?;
    drop(stmt);
    batches.entries.clear();
    Ok(())
}

/// Flushes buffered term rows.
fn flush_terms(tx: &rusqlite::Transaction, text: &str, batches: &mut Batches) -> Result<()> {
    let batch = &batches.terms;
    if batch.is_empty() {
        return Ok(());
    }
    let owned;
    let sql = if batch.len() == BATCH_ROWS {
        &batches.term_sql
    } else {
        owned = insert_sql(TERM_INSERT, 7, batch.len());
        &owned
    };
    let mut stmt = tx.prepare_cached(sql)?;
    let mut idx = 1;
    for row in batch.iter() {
        stmt.raw_bind_parameter(idx, slice(text, row.0))?;
        stmt.raw_bind_parameter(idx + 1, row.1.map(|s| slice(text, s)))?;
        stmt.raw_bind_parameter(idx + 2, slice(text, row.2))?;
        stmt.raw_bind_parameter(idx + 3, slice(text, row.3))?;
        stmt.raw_bind_parameter(idx + 4, row.4)?;
        stmt.raw_bind_parameter(idx + 5, row.5)?;
        stmt.raw_bind_parameter(idx + 6, row.6)?;
        idx += 7;
    }
    stmt.raw_execute()?;
    drop(stmt);
    batches.terms.clear();
    Ok(())
}

/// Creates the table schema.
fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta (k, v) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Creates every index that does not exist.
///
/// The connection must allow writes.
pub fn ensure_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(INDEXES).context("creating the term indexes")?;
    Ok(())
}

/// A dictionary's title.
fn dict_title(archive: &Path) -> Result<String> {
    let idx = read_index(archive)?;
    let title = idx.get("title").and_then(|v| v.as_str()).map(str::to_string);
    Ok(title.unwrap_or_else(|| stem(archive)))
}

/// A path's file stem.
fn stem(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string()
}

/// Records the build's sources and its time in `meta`.
fn write_meta(conn: &Connection, terms: &[PathBuf], freqs: &[PathBuf]) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at', ?1)",
        params![now_iso_utc()],
    )?;

    let mut sources = Vec::new();
    for path in terms.iter().chain(freqs.iter()) {
        sources.push(source_hash(path)?);
    }
    conn.execute(
        "INSERT OR REPLACE INTO meta (k, v) VALUES ('source_hashes', ?1)",
        params![to_py_json(&sources)?],
    )?;
    Ok(())
}

/// One source's hash record.
#[derive(Serialize)]
pub(crate) struct SourceHash {
    name: String,
    bytes: u64,
    sha256: String,
}

/// One archive's meta record.
pub(crate) fn source_hash(path: &Path) -> Result<SourceHash> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let bytes = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    Ok(SourceHash { name, bytes, sha256: hash_file(path)? })
}

/// Returns the lowercase hexadecimal SHA-256 of a file.
///
/// This is the crate's only hash. `meta.source_hashes` records it for every
/// archive. [`crate::library::Library`] uses it to distinguish two names for
/// one Dictionary from two different Dictionaries.
/// This function reads 64 KiB blocks. The library sized its gate for the
/// measured rate of about 400 MiB per second.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Lowercase hex of bytes.
fn to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Incremental SHA-256 state.
struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Adds more bytes to the hash.
    fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                process_block(&mut self.state, &block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            process_block(&mut self.state, &block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Ends the hash and returns it.
    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        self.update(&bit_len.to_be_bytes());

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Runs one compression round.
#[allow(clippy::many_single_char_names)]
fn process_block(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Current UTC time as text.
fn now_iso_utc() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format_iso_utc(secs)
}

/// Formats epoch seconds.
fn format_iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (y, mo, d) = civil_from_days(days);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}+00:00")
}

/// Epoch days to a civil date.
fn civil_from_days(z_in: i64) -> (i64, u32, u32) {
    let z = z_in + 719_468;
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe: u64 = (z - era * 146_097) as u64;
    let yoe: u64 = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y: i64 = yoe as i64 + era * 400;
    let doy: u64 = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp: u64 = (5 * doy + 2) / 153;
    let d: u32 = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m: u32 = if mp < 10 { (mp + 3) as u32 } else { (mp - 9) as u32 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::gloss::{plain_items, RoleFilter, Selection};
    use rusqlite::OpenFlags;

    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    fn out_path(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_build_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn build_fixture_db(test_name: &str) -> (Connection, TempDbGuard) {
        let out = out_path(test_name);
        let guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out, &|_| {}).unwrap();
        (Connection::open(&out).unwrap(), guard)
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        to_hex(&hasher.finalize())
    }

    /// Returns the glossary record that a hover reads.
    fn stored_glossary(conn: &Connection, surface: &str) -> String {
        conn.query_row(
            "SELECT glossary FROM entry JOIN term USING(entry_id) WHERE surface = ?1",
            [surface],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Returns the `GlossDoc` that a hover parses from the record.
    fn stored_doc(conn: &Connection, surface: &str) -> GlossDoc {
        GlossDoc::parse(&stored_glossary(conn, surface))
    }

    #[test]
    fn entry_and_term_counts() {
        let (conn, _guard) = build_fixture_db("entry_and_term_counts");
        let entries: i64 =
            conn.query_row("SELECT COUNT(*) FROM entry", [], |r| r.get(0)).unwrap();
        let terms: i64 = conn.query_row("SELECT COUNT(*) FROM term", [], |r| r.get(0)).unwrap();
        assert_eq!(3, entries);
        assert_eq!(5, terms);
    }

    #[test]
    fn dictionary_name_comes_from_the_index() {
        let (conn, _guard) = build_fixture_db("dictionary_name_comes_from_the_index");
        let name: String = conn.query_row("SELECT name FROM dict", [], |r| r.get(0)).unwrap();
        assert_eq!("FixtureTerms", name);
    }

    /// The record keeps the Dictionary's structured content, not a rendered form.
    /// A renderer or parser fix can then reach users through a patch instead of a rebuild.
    #[test]
    fn the_record_stores_the_raw_structured_content() {
        let (conn, _guard) = build_fixture_db("the_record_stores_the_raw_structured_content");
        let stored: serde_json::Value =
            serde_json::from_str(&stored_glossary(&conn, "食べる")).unwrap();
        assert_eq!("structured-content", stored[0]["type"]);
        assert!(
            stored.to_string().contains("part-of-speech-info"),
            "the part-of-speech markup is kept, not lifted out at build time: {stored}"
        );
    }

    #[test]
    fn structured_content_renders_to_one_gloss() {
        let (conn, _guard) = build_fixture_db("structured_content_renders_to_one_gloss");
        assert_eq!(vec!["to eat".to_string()], plain_items(&stored_doc(&conn, "食べる")));
    }

    #[test]
    fn structured_content_also_renders_as_html() {
        let (conn, _guard) = build_fixture_db("structured_content_also_renders_as_html");
        assert_eq!(
            vec!["<span>to eat</span>".to_string()],
            crate::dict::gloss::render_html(
                &stored_doc(&conn, "食べる"),
                Selection::Whole,
                RoleFilter::CARD,
            )
        );
    }

    #[test]
    fn a_plain_string_glossary_gets_a_matching_html_rendering() {
        let (conn, _guard) =
            build_fixture_db("a_plain_string_glossary_gets_a_matching_html_rendering");
        let doc = stored_doc(&conn, "ねこ");
        assert_eq!(vec!["cat".to_string()], plain_items(&doc));
        assert_eq!(
            vec!["cat".to_string()],
            crate::dict::gloss::render_html(&doc, Selection::Whole, RoleFilter::CARD)
        );
    }

    #[test]
    fn part_of_speech_spans_are_separated_from_glosses() {
        let (conn, _guard) = build_fixture_db("part_of_speech_spans_are_separated_from_glosses");
        assert_eq!(
            vec!["1-dan".to_string(), "transitive".to_string()],
            crate::dict::gloss::pos_labels(&stored_doc(&conn, "食べる"))
        );
    }

    #[test]
    fn rules_field_lands_in_the_term_pos_column() {
        let (conn, _guard) = build_fixture_db("rules_field_lands_in_the_term_pos_column");
        let pos: String = conn
            .query_row("SELECT pos FROM term WHERE surface = ?1", ["食べる"], |r| r.get(0))
            .unwrap();
        assert_eq!("v1", pos);
    }

    #[test]
    fn kana_only_headword_has_null_written_and_freq() {
        let (conn, _guard) = build_fixture_db("kana_only_headword_has_null_written_and_freq");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM term WHERE surface = ?1 AND written IS NULL",
                ["ねこ"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(1, n);
        let freq: Option<i64> = conn
            .query_row(
                "SELECT freq FROM term WHERE surface = ?1 AND written IS NULL",
                ["ねこ"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(None, freq);
    }

    #[test]
    fn reading_agnostic_frequency_is_applied() {
        let (conn, _guard) = build_fixture_db("reading_agnostic_frequency_is_applied");
        let freq: i64 = conn
            .query_row("SELECT freq FROM term WHERE surface = ?1", ["食べる"], |r| r.get(0))
            .unwrap();
        assert_eq!(7, freq);
    }

    #[test]
    fn reading_scoped_frequency_beats_reading_agnostic() {
        let (conn, _guard) = build_fixture_db("reading_scoped_frequency_beats_reading_agnostic");
        let freq: i64 = conn
            .query_row("SELECT freq FROM term WHERE surface = ?1", ["猫"], |r| r.get(0))
            .unwrap();
        assert_eq!(42, freq);
        assert_ne!(9999, freq);
    }

    #[test]
    fn sha256_matches_the_empty_string_vector() {
        assert_eq!(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            sha256_hex(b"")
        );
    }

    #[test]
    fn sha256_matches_the_abc_vector() {
        assert_eq!(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            sha256_hex(b"abc")
        );
    }

    #[test]
    fn sha256_matches_a_multi_block_vector() {
        assert_eq!(
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")
        );
    }

    #[test]
    fn sha256_of_the_terms_fixture_matches_hashlib() {
        let hex = hash_file(&fixture("terms.zip")).unwrap();
        assert_eq!("b1a8876d676bcea6accb3e1f0c1c20b539cebad7652723108b0b2538ab4056a6", hex);
    }

    #[test]
    fn sha256_of_the_freq_fixture_matches_hashlib() {
        let hex = hash_file(&fixture("freq.zip")).unwrap();
        assert_eq!("d49ca40eb1d1f3d32cc7e49162405255a98569078fa2f459a9fe9260ed54fbdc", hex);
    }

    #[test]
    fn the_unix_epoch_formats_correctly() {
        assert_eq!("1970-01-01T00:00:00+00:00", format_iso_utc(0));
    }

    #[test]
    fn a_leap_day_formats_correctly() {
        assert_eq!("2000-02-29T12:30:45+00:00", format_iso_utc(951_827_445));
    }

    #[test]
    fn a_year_boundary_formats_correctly() {
        assert_eq!("2024-12-31T23:59:59+00:00", format_iso_utc(1_735_689_599));
    }

    #[test]
    fn an_independently_computed_epoch_formats_correctly() {
        assert_eq!("2026-07-29T09:15:42+00:00", format_iso_utc(1_785_316_542));
    }

    #[test]
    fn an_empty_term_list_is_refused_rather_than_built_empty() {
        let out = out_path("empty_terms");
        let _guard = TempDbGuard(out.clone());
        std::fs::write(&out, b"PRECIOUS").unwrap();

        assert!(build(&[], &[], &out, &|_| {}).is_err(), "an empty build must not be attempted");
        assert_eq!(b"PRECIOUS".to_vec(), std::fs::read(&out).unwrap(), "output untouched");
    }

    #[test]
    fn a_failed_build_leaves_the_previous_database_intact() {
        let out = out_path("failed_build");
        let _guard = TempDbGuard(out.clone());
        std::fs::write(&out, b"PRECIOUS").unwrap();
        let bad = out_path("failed_build_src");
        let _bad_guard = TempDbGuard(bad.clone());
        std::fs::write(&bad, b"not a zip at all").unwrap();

        assert!(build(std::slice::from_ref(&bad), &[], &out, &|_| {}).is_err());
        assert_eq!(b"PRECIOUS".to_vec(), std::fs::read(&out).unwrap(), "output untouched");
        assert!(!suffixed(&out, ".building").exists(), "no .building left behind");
    }

    #[test]
    fn a_missing_output_directory_is_created() {
        let dir = std::env::temp_dir()
            .join("chibipop_build_test")
            .join(format!("fresh_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A fresh executable has no `data/` directory.
        let out = dir.join("data").join("chibipop.sqlite");

        let counts = build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out, &|_| {})
            .expect("a fresh install must be able to build");

        assert_eq!(3, counts.entries);
        assert!(out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_build_leaves_no_building_file() {
        let (_conn, guard) = build_fixture_db("no_building_left");
        assert!(!suffixed(&guard.0, ".building").exists());
    }

    // ---- promote a finished build over the live database ----

    /// Gets the named Dictionary from a database. These tests use it to
    /// distinguish the old file from the new file.
    fn first_dict(db: &Path) -> String {
        let conn = Connection::open(db).unwrap();
        conn.query_row("SELECT name FROM dict WHERE dict_id = 1", [], |r| r.get(0)).unwrap()
    }

    fn verdict(db: &Path) -> String {
        let conn = Connection::open(db).unwrap();
        conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap()
    }

    fn marker(db: &Path) -> Option<String> {
        use rusqlite::OptionalExtension as _;
        let conn = Connection::open(db).unwrap();
        conn.query_row("SELECT v FROM meta WHERE k = 'marker'", [], |r| r.get(0))
            .optional()
            .unwrap()
    }

    /// Creates a database and a sidecar log that a crashed writer left beside it.
    ///
    /// The helper copies both files under the test name. It does not keep the
    /// copied pair open. SQLite keys `-wal` to the database's **file name**, so
    /// the pair reproduces that crash state.
    /// A fixture with an open writer tests a platform rule instead. Windows
    /// refuses to unlink or rename an open file because SQLite opens without
    /// `FILE_SHARE_DELETE`. Promotion cannot run beside a live handle
    /// (docs/BACKLOG.md, "Windows will not rename onto an open file").
    ///
    /// The helper grows the database beyond one page. A stale log can then
    /// conflict with a differently laid out file and produce
    /// `database disk image is malformed`. A one-page database has no page layout
    /// conflict.
    ///
    /// The marker commit occurs after the checkpoint. The log then holds frames
    /// that the checkpoint did not copy, which is the state that a killed
    /// daemon leaves behind.
    fn crashed_with_a_log(out: &Path) {
        let source = suffixed(out, ".source");
        let source_guard = TempDbGuard(source.clone());
        build(&[fixture("terms.zip")], &[], &source, &|_| {}).expect("the first build");
        {
            let live = Connection::open(&source).unwrap();
            live.execute_batch(
                "INSERT INTO meta (k, v)
                   WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 4000)
                   SELECT 'pad' || i, hex(zeroblob(100)) FROM s;
                 PRAGMA wal_checkpoint(TRUNCATE);
                 INSERT OR REPLACE INTO meta (k, v) VALUES ('marker', 'old');",
            )
            .unwrap();
            assert!(suffixed(&source, "-wal").exists(), "the log is what this test is about");
            // Copy the files while the writer is idle between commits. Disk then
            // contains exactly what a killed writer leaves.
            for suffix in ["", "-wal", "-shm"] {
                let from = suffixed(&source, suffix);
                if from.exists() {
                    std::fs::copy(&from, suffixed(out, suffix)).expect("the crash is copied");
                }
            }
        }
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(suffixed(&source, suffix));
        }
        drop(source_guard);
        assert!(suffixed(out, "-wal").exists(), "the copy keeps the log beside the database");
    }

    /// Holds the same database state that a daemon leaves behind: a sidecar log
    /// has frames that the database does not yet contain.
    ///
    /// Every step matters. The reader starts its snapshot before the final
    /// commit. The read-only reader cannot copy that commit, drain the log, or
    /// delete it. The log outlives the writer with live frames.
    /// A fully copied log is safe. Its index says that it has no frames to
    /// replay, and a reader skips it.
    #[cfg(unix)]
    fn held_by_a_reader(out: &Path) -> Connection {
        build(&[fixture("terms.zip")], &[], out, &|_| {}).expect("the first build");
        let writer = Connection::open(out).unwrap();
        writer
            .execute_batch(
                "INSERT INTO meta (k, v)
                   WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 120)
                   SELECT 'pad' || i, hex(zeroblob(100)) FROM s;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        let reader = Connection::open_with_flags(out, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        reader.execute_batch("BEGIN; SELECT COUNT(*) FROM meta;").unwrap();
        writer
            .execute_batch("INSERT OR REPLACE INTO meta (k, v) VALUES ('marker', 'old');")
            .unwrap();
        drop(writer);
        assert!(suffixed(out, "-wal").exists(), "a held-open reader keeps the log alive");
        reader
    }


    /// A call to [`promote`] must leave no sidecar from the replaced database.
    ///
    /// SQLite keys WAL sidecars to the database **file name**, not its inode.
    /// `rename` replaces only the main file. If promotion leaves the old log,
    /// the next cold reader can recover old pages into the new database.
    /// That recovery can write old pages over new pages and return
    /// `database disk image is malformed`.
    /// A crashed writer with no open handle is the Windows case. [`drain_wal`]
    /// already empties that log.
    /// This test states the invariant but does not exercise the failure.
    /// The failure uses a log that a checkpoint cannot drain. It requires a live
    /// handle and appears in the POSIX-only test below.
    #[test]
    fn a_promote_never_leaves_the_previous_databases_log_beside_the_new_one() {
        let out = out_path("stale_log");
        let guard = TempDbGuard(out.clone());
        crashed_with_a_log(&out);

        // Build a different Dictionary while the old log remains on disk. This
        // matches a rebuild after the writer process dies.
        build(&[media_archive()], &[], &out, &|_| {}).expect("the rebuild promotes");

        assert!(!suffixed(&out, "-wal").exists(), "the old log must not outlive the promote");
        assert!(!suffixed(&out, "-shm").exists(), "nor its index");

        // Open the promoted file with a cold reader. This matches the user-visible
        // symptom.
        assert_eq!("ok", verdict(&out), "a cold reader must find the promoted file sound");
        assert_eq!("FixtureMedia", first_dict(&out), "it is the dictionary just built");
        assert_eq!(None, marker(&out), "with nothing recovered out of the old log");
        drop(guard);
    }

    /// Reproduces the user's bug and covers [`drop_sidecars`].
    ///
    /// The old database stays open. This matches a Linux rebuild.
    /// The daemon is another process. Its reader holds a snapshot that the
    /// checkpoint cannot copy. The log has live frames when promotion renames the file.
    /// Without [`drop_sidecars`], this test reports
    /// `wrong # of entries in index sqlite_autoindex_meta_1` from
    /// `integrity_check`. The user's database reported
    /// `Tree 3 page 3 cell 0: invalid page number`.
    /// The exact page error depends on which new pages receive old log frames.
    /// The invariant matters more than the error text.
    ///
    /// This test runs on POSIX only. Windows refuses unlink and rename when a
    /// handle holds the file. The tray app edits its database in place, and its
    /// rebuild process opens no database (docs/BACKLOG.md, docs/REGRESSION.md 1.20).
    #[cfg(unix)]
    #[test]
    fn a_promote_under_a_live_reader_leaves_no_log_behind_either() {
        let out = out_path("stale_log_live");
        let guard = TempDbGuard(out.clone());
        let live = held_by_a_reader(&out);

        build(&[media_archive()], &[], &out, &|_| {}).expect("the rebuild promotes");

        // Check this invariant while the daemon handle remains open.
        assert!(!suffixed(&out, "-wal").exists(), "the old log must not outlive the promote");
        assert!(!suffixed(&out, "-shm").exists(), "nor its index");

        drop(live);
        assert_eq!("ok", verdict(&out), "a cold reader must find the promoted file sound");
        assert_eq!("FixtureMedia", first_dict(&out), "it is the dictionary just built");
        assert_eq!(None, marker(&out), "with nothing recovered out of the old log");
        drop(guard);
    }

    /// Checks the crash window that the order in [`promote`] protects.
    /// Between sidecar removal and rename, the old database must remain complete
    /// and readable.
    /// The checkpoint before removal provides that state. Removal after rename
    /// exposes the new file to the old log.
    #[test]
    fn a_promote_interrupted_before_the_rename_leaves_the_old_database_whole() {
        let out = out_path("interrupted");
        let guard = TempDbGuard(out.clone());
        crashed_with_a_log(&out);
        let tmp = suffixed(&out, ".building");
        let tmp_guard = TempDbGuard(tmp.clone());
        build_into(&[media_archive()], &[], &tmp, &|_| {}).expect("the new build");

        verify_built(&tmp, &|_| {}).expect("the new build checks out");
        drain_wal(&out);
        drop_sidecars(&out).expect("the old log goes");
        // The process dies here, one system call before rename.

        assert_eq!("ok", verdict(&out), "the old database has to still be readable");
        assert_eq!("FixtureTerms", first_dict(&out), "and still be the old one");
        assert_eq!(
            Some("old".to_string()),
            marker(&out),
            "the checkpoint is what keeps the log's own rows: dropping a log \
             that had not been drained is how a promote loses a transaction",
        );
        drop(tmp_guard);
        drop(guard);
    }

    /// Checks the promotion gate.
    /// `build_into` uses `synchronous = OFF`, so a killed build can leave a file
    /// that opens but cannot be read. A promotion of that file replaces a valid
    /// Dictionary with a damaged one.
    /// This test calls [`promote`] directly because a completed `build` cannot
    /// produce a torn page.
    #[test]
    fn a_build_that_does_not_check_out_is_refused_rather_than_promoted() {
        let out = out_path("torn_build");
        let guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out, &|_| {}).unwrap();
        let good = std::fs::read(&out).unwrap();

        // Zero a page inside a completed build. Page 1 only tests header validation,
        // because SQLite rejects that file at open. A later page tests a file that
        // opens but is not a valid database.
        let tmp = suffixed(&out, ".building");
        let tmp_guard = TempDbGuard(tmp.clone());
        let mut torn = good.clone();
        assert!(torn.len() > 3 * 8192, "the fixture has to be more than three pages");
        torn[2 * 8192..3 * 8192].fill(0);
        std::fs::write(&tmp, &torn).unwrap();

        let err = promote(&tmp, &out, &|_| {}).expect_err("a torn build must not be promoted");
        assert!(format!("{err:#}").contains("did not check out"), "got: {err:#}");
        assert_eq!(good, std::fs::read(&out).unwrap(), "the working database is untouched");
        drop(tmp_guard);
        drop(guard);
    }

    #[test]
    fn build_with_fixture_is_query_identical_across_runs() {
        let out1 = out_path("identical_a");
        let out2 = out_path("identical_b");
        let _g1 = TempDbGuard(out1.clone());
        let _g2 = TempDbGuard(out2.clone());

        build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out1, &|_| {}).unwrap();
        build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out2, &|_| {}).unwrap();

        let c1 = Connection::open(&out1).unwrap();
        let c2 = Connection::open(&out2).unwrap();

        let count = |c: &Connection, t: &str| -> i64 {
            c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()
        };
        for table in ["dict", "entry", "term"] {
            assert_eq!(count(&c1, table), count(&c2, table), "{table} row count mismatch");
        }
    }

    fn ints(conn: &Connection, sql: &str) -> Vec<i64> {
        let mut stmt = conn.prepare(sql).unwrap();
        let got = stmt.query_map([], |r| r.get(0)).unwrap();
        got.collect::<rusqlite::Result<Vec<i64>>>().unwrap()
    }

    fn cells(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(sql).unwrap();
        let width = stmt.column_count();
        let got = stmt
            .query_map([], |r| {
                let row: Vec<String> =
                    (0..width).map(|i| format!("{:?}", r.get_ref_unwrap(i))).collect();
                Ok(row.join("|"))
            })
            .unwrap();
        got.collect::<rusqlite::Result<Vec<String>>>().unwrap()
    }

    /// Pins the per-archive seam.
    #[test]
    fn a_two_archive_build_numbers_each_archive_after_the_last() {
        let out = out_path("two_archive_seam");
        let _guard = TempDbGuard(out.clone());
        let terms = [fixture("terms.zip"), fixture("terms.zip")];

        let counts = build(&terms, &[fixture("freq.zip")], &out, &|_| {}).unwrap();

        assert_eq!(6, counts.entries);
        assert_eq!(10, counts.terms);
        let conn = Connection::open(&out).unwrap();
        // Two term archives come first, then the frequency Dictionary. Each term
        // archive keeps its `dict_id` at its position in `terms`, and `freq.zip`
        // follows as a Dictionary of its own.
        assert_eq!(vec![1, 2, 3], ints(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"));
        assert_eq!(vec![0, 1, 2], ints(&conn, "SELECT priority FROM dict ORDER BY dict_id"));
        let freq_name: String =
            conn.query_row("SELECT name FROM dict WHERE dict_id = 3", [], |r| r.get(0)).unwrap();
        assert_eq!("FixtureFreq", freq_name);
        assert_eq!(
            vec![1, 2, 3],
            ints(&conn, "SELECT entry_id FROM entry WHERE dict_id = 1 ORDER BY entry_id")
        );
        assert_eq!(
            vec![4, 5, 6],
            ints(&conn, "SELECT entry_id FROM entry WHERE dict_id = 2 ORDER BY entry_id")
        );
        assert_eq!(
            (1..=5).collect::<Vec<i64>>(),
            ints(&conn, "SELECT rowid FROM term WHERE dict_id = 1 ORDER BY rowid")
        );
        assert_eq!(
            (6..=10).collect::<Vec<i64>>(),
            ints(&conn, "SELECT rowid FROM term WHERE dict_id = 2 ORDER BY rowid")
        );

        let cols = "surface, written, reading, pos, freq";
        assert_eq!(
            cells(&conn, &format!("SELECT {cols} FROM term WHERE dict_id = 1 ORDER BY rowid")),
            cells(&conn, &format!("SELECT {cols} FROM term WHERE dict_id = 2 ORDER BY rowid")),
            "the same archive twice must parse to the same term rows"
        );
        assert_eq!(
            cells(&conn, "SELECT glossary FROM entry WHERE dict_id = 1 ORDER BY entry_id"),
            cells(&conn, "SELECT glossary FROM entry WHERE dict_id = 2 ORDER BY entry_id"),
            "and to the same glossary records"
        );
    }

    /// The concurrency invariant that this test protects.
    ///
    /// `banks.zip` has twelve term banks. One bank is one hundred times larger
    /// than the others, so eleven banks finish before bank 3.
    /// The writer must still process every bank in archive order. It assigns
    /// `entry_id` in that order. Completion order changes the database on each
    /// run and breaks every caller that holds an `entry_id`.
    ///
    /// Assert the full sequence, not only a count. A count passes for any order.
    #[test]
    fn a_many_bank_archive_is_numbered_in_archive_order() {
        let out = out_path("many_banks");
        let _guard = TempDbGuard(out.clone());

        let counts = build(&[fixture("banks.zip")], &[], &out, &|_| {}).unwrap();

        // Eleven banks have four rows each. One bank has four hundred rows.
        assert_eq!(11 * 4 + 400, counts.entries);
        let conn = Connection::open(&out).unwrap();
        let mut stmt = conn.prepare("SELECT surface FROM term ORDER BY rowid").unwrap();
        let surfaces: Vec<String> =
            stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        // Each entry has two `term` rows, with the reading first. These headwords use
        // kanji, so no entry collapses to one row.
        let expected: Vec<String> = (1..=12)
            .flat_map(|bank| {
                let rows = if bank == 3 { 400 } else { 4 };
                (0..rows).flat_map(move |row| {
                    [format!("b{bank:02}r{row:03}"), format!("b{bank:02}w{row:03}")]
                })
            })
            .collect();
        assert_eq!(expected, surfaces, "banks 10-12 follow 9, and bank 3 holds its place");
    }

    /// Two threaded builds must produce identical rows in every table that a
    /// reader uses. `meta` contains a timestamp, so this test excludes that value
    /// rather than change the comparison.
    #[test]
    fn a_many_bank_build_is_reproducible() {
        let one = out_path("many_banks_a");
        let two = out_path("many_banks_b");
        let _g1 = TempDbGuard(one.clone());
        let _g2 = TempDbGuard(two.clone());
        let archives = [fixture("banks.zip"), fixture("terms.zip")];

        build(&archives, &[fixture("freq.zip")], &one, &|_| {}).unwrap();
        build(&archives, &[fixture("freq.zip")], &two, &|_| {}).unwrap();

        let sql = "SELECT entry_id, dict_id, glossary FROM entry ORDER BY entry_id";
        let terms = "SELECT rowid, surface, written, reading, pos, freq, entry_id, dict_id \
                     FROM term ORDER BY rowid";
        let a = Connection::open(&one).unwrap();
        let b = Connection::open(&two).unwrap();
        assert_eq!(cells(&a, sql), cells(&b, sql));
        assert_eq!(cells(&a, terms), cells(&b, terms));
    }

    /// The stored glossary keeps the archive's JSON bytes without whitespace
    /// between tokens. It also keeps key order.
    /// The old writer passed each glossary through `serde_json::Value`, which
    /// sorted map keys. The build never needed that sort.
    #[test]
    fn the_stored_glossary_is_the_archives_own_json_minified() {
        assert_eq!(
            r#"{"type":"text","text":"a b","n":[1,2]}"#,
            minified(r#"{ "type": "text", "text": "a b", "n": [ 1, 2 ] }"#)
        );
    }

    /// Whitespace inside a string is content, not layout.
    #[test]
    fn minifying_leaves_string_contents_alone() {
        assert_eq!(r#"["a\n b","c \\","  "]"#, minified(r#"[ "a\n b", "c \\" , "  " ]"#));
    }

    fn minified(json: &str) -> String {
        let mut out = String::new();
        minify_json(json, &mut out);
        // Minification must never change the meaning of JSON.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json).unwrap(),
            serde_json::from_str::<serde_json::Value>(&out).unwrap(),
        );
        out
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
            .unwrap();
        let names = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        names.collect::<rusqlite::Result<Vec<String>>>().unwrap()
    }

    #[test]
    fn a_fresh_build_indexes_term_entry_id() {
        let (conn, _guard) = build_fixture_db("fresh_build_entry_id_index");
        let names = index_names(&conn);
        assert!(
            names.iter().any(|n| n == "idx_term_entry_id"),
            "a new database must carry the entry_id index: {names:?}"
        );
    }

    #[test]
    fn the_ensure_adds_the_entry_id_index_and_repeats_cleanly() {
        let (conn, _guard) = build_fixture_db("ensure_adds_entry_id_index");
        conn.execute_batch("DROP INDEX idx_term_entry_id;").unwrap();
        assert!(
            !index_names(&conn).iter().any(|n| n == "idx_term_entry_id"),
            "a database built before v0.8.0 has no entry_id index"
        );

        ensure_indexes(&conn).unwrap();
        let after = index_names(&conn);
        assert!(after.iter().any(|n| n == "idx_term_entry_id"), "not created: {after:?}");

        ensure_indexes(&conn).expect("a second ensure must not error");
        assert_eq!(after, index_names(&conn), "a second ensure must change nothing");
    }

    /// `ensure_indexes` must not use the lookup connection.
    #[test]
    fn the_ensure_refuses_a_read_only_connection() {
        let (conn, guard) = build_fixture_db("ensure_read_only");
        conn.execute_batch("DROP INDEX idx_term_entry_id;").unwrap();

        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let read_only = Connection::open_with_flags(&guard.0, flags).unwrap();

        let err = ensure_indexes(&read_only).expect_err("a read-only connection cannot index");
        let chain = format!("{err:#}").to_lowercase();
        assert!(
            chain.contains("readonly"),
            "the ensure needs a writable connection: {chain}"
        );
    }

    // ---- the Media store ----

    fn media_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/media.zip")
    }

    /// Builds the media fixture with the real builder and records its progress
    /// lines.
    fn build_media_db(test_name: &str) -> (Connection, TempDbGuard, BuildCounts, Vec<String>) {
        let out = out_path(test_name);
        let guard = TempDbGuard(out.clone());
        let lines = std::cell::RefCell::new(Vec::new());
        let counts = build(&[media_archive()], &[], &out, &|line| {
            lines.borrow_mut().push(line.to_string());
        })
        .expect("the media fixture builds");
        (Connection::open(&out).unwrap(), guard, counts, lines.into_inner())
    }

    fn media_paths(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT path FROM media ORDER BY path").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    /// If the build extracts every archive asset, it stores the full glyph set for
    /// an image Dictionary. 30 of 52 structured-content Dictionaries emit images,
    /// but the build can paint only paths that image nodes name.
    #[test]
    fn only_the_assets_an_image_node_references_are_extracted() {
        let (conn, _guard, counts, _lines) = build_media_db("media_only_referenced");
        assert_eq!(
            vec![
                "gaiji/copy.png",
                "gaiji/five.avif",
                "gaiji/four.gif",
                "gaiji/one.png",
                "gaiji/ratio.svg",
                "gaiji/three.jpg",
                // Size comes from the header, but the decoder fails. The row supports the
                // `alt`-text fallback at paint time.
                "gaiji/torn.png",
                "gaiji/two.svg",
            ],
            media_paths(&conn),
        );
        // The archive contains `unused.png`, but no node names it.
        assert_eq!(8, counts.media.stored);
        assert_eq!(10, counts.media.referenced, "eight stored, one absent, one unsizeable");
    }

    /// An image-only glossary does not render text, so it does not create an entry
    /// or media that a hover can reach.
    #[test]
    fn an_image_only_term_row_contributes_no_media() {
        let (conn, _guard, ..) = build_media_db("media_image_only_row");
        assert!(
            !media_paths(&conn).iter().any(|p| p == "gaiji/dropped.png"),
            "the archive ships it and only a dropped row references it",
        );
    }

    /// 字通 averages more than four image nodes per term row across a few
    /// thousand distinct gaiji. Shared bytes reduce import cost. This is not an
    /// optimization.
    #[test]
    fn two_identical_assets_at_different_paths_share_one_blob() {
        let (conn, _guard, counts, _lines) = build_media_db("media_dedup");
        let blob_of = |path: &str| -> i64 {
            conn.query_row("SELECT blob_id FROM media WHERE path = ?1", [path], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            blob_of("gaiji/one.png"),
            blob_of("gaiji/copy.png"),
            "identical bytes at two paths are one blob",
        );
        let blobs: i64 =
            conn.query_row("SELECT COUNT(*) FROM media_blob", [], |r| r.get(0)).unwrap();
        assert_eq!(7, blobs, "eight media rows over seven distinct assets");
        assert_eq!(7, counts.media.blobs);
    }

    /// These columns are part of the schema required for import. 99 807 census
    /// image nodes declare neither `width` nor `height`. A wrong value mismeasures a
    /// line. It affects layout, not only the picture.
    #[test]
    fn the_intrinsic_size_of_every_supported_format_is_recorded() {
        let (conn, _guard, ..) = build_media_db("media_intrinsics");
        let recorded = |path: &str| -> (String, f64, f64, f64) {
            conn.query_row(
                "SELECT format, width, height, aspect FROM media WHERE path = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        for (path, format, w, h) in [
            ("gaiji/one.png", "png", 12.0, 7.0),
            ("gaiji/three.jpg", "jpeg", 23.0, 11.0),
            // Record the logical screen of a two-frame animation, not its 4x3 frames.
            // The screen is the canvas that a browser lays out.
            ("gaiji/four.gif", "gif", 9.0, 5.0),
            ("gaiji/two.svg", "svg", 64.0, 32.0),
            // The root element has no width or height. The `viewBox` supplies the size,
            // which is common for a gaiji SVG.
            ("gaiji/ratio.svg", "svg", 100.0, 40.0),
            // The `avif` format stores its size in an item property, not a header. This
            // build cannot rasterize it, so it must record the size here.
            ("gaiji/five.avif", "avif", 480.0, 120.0),
        ] {
            let (got_format, got_w, got_h, got_aspect) = recorded(path);
            assert_eq!(format, got_format, "{path}");
            assert_eq!((w, h), (got_w, got_h), "{path}");
            // Use `f32` for this comparison because the row uses that type. Popup
            // geometry also uses `f32`, and `9 / 5` is not exact.
            assert_eq!(
                w as f32 / h as f32,
                got_aspect as f32,
                "{path}: aspect is a column, not a derivation",
            );
        }
    }

    /// The `alt`-text ladder creates a media row only when bytes are stored and
    /// size is known. A lookup with no row therefore means "fall back".
    #[test]
    fn a_missing_or_unreadable_asset_is_counted_and_never_fails_the_build() {
        let (conn, _guard, counts, lines) = build_media_db("media_absent");
        let paths = media_paths(&conn);
        assert!(!paths.iter().any(|p| p == "gaiji/missing.png"), "absent from the archive");
        assert!(!paths.iter().any(|p| p == "gaiji/broken.png"), "present and unsizeable");
        assert_eq!(1, counts.media.missing);
        assert_eq!(1, counts.media.unreadable);
        // The entry that names them remains an entry, with its text intact.
        assert_eq!(vec!["fish".to_string()], plain_items(&stored_doc(&conn, "さかな")));

        // Name each path so the Dictionary author can act on it.
        let joined = lines.join("\n");
        assert!(joined.contains("gaiji/broken.png"), "the corrupt one is named: {joined}");
        assert!(joined.contains("gaiji/missing.png"), "the absent one is named: {joined}");
    }

    #[test]
    fn the_media_diagnostic_names_the_dictionary_and_its_counts() {
        let (.., lines) = build_media_db("media_diagnostic");
        let per_dict = lines.iter().find(|l| l.contains("FixtureMedia"));
        let per_dict = per_dict.unwrap_or_else(|| panic!("no per-dictionary line: {lines:?}"));
        assert!(
            per_dict.contains("8 of 10 assets in 7 blobs"),
            "the line has to carry the numbers: {per_dict}",
        );
        assert!(
            lines.iter().any(|l| l.contains("all dictionaries")),
            "and the corpus total: {lines:?}",
        );
    }

    /// An archive without image nodes costs no media work. It needs no second
    /// ZIP pass, writes no rows, and emits no diagnostic.
    #[test]
    fn a_dictionary_with_no_image_nodes_writes_no_media_and_says_nothing() {
        let (conn, _guard) = build_fixture_db("media_none");
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0)).unwrap();
        assert_eq!(0, rows);
    }

    // ---- a Dictionary's own styles.css ----

    /// Creates one archive with a stylesheet and builds it with the real builder.
    ///
    /// `index.json` stays at the root because `read_index` requires that path.
    /// The stylesheet stays one directory deep, which is the second location
    /// that the census found.
    fn styled_archive(name: &str, css: &str) -> PathBuf {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("chibipop_build_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("a_{}_{name}.zip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("index.json", opts).unwrap();
        zip.write_all(br#"{"title":"Styled","format":3,"revision":"1"}"#).unwrap();
        zip.start_file("term_bank_1.json", opts).unwrap();
        zip.write_all(
            br#"[["\u66f8\u304f","\u304b\u304f","","v5k",0,
                 [{"type":"structured-content","content":
                   {"tag":"span","data":{"fbox":"1"},"content":"to write"}}],
                 0,""]]"#,
        )
        .unwrap();
        zip.start_file("assets/styles.css", opts).unwrap();
        zip.write_all(css.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    /// The build stores stylesheet *text* and reports the rules that compilation
    /// keeps or drops. It stores no compiled form because the matcher compiles
    /// text on first use. A matcher fix needs a patch, not a rebuild.
    /// The compile fills this report only. `dropped` is the grammar-gap count
    /// that `tools/dict-census` checks.
    #[test]
    fn a_build_stores_the_stylesheet_and_reports_what_it_dropped() {
        let css = "span[data-sc-fbox] { padding: 0.1em }\n\
                   .gloss-image { border: 1px }\n\
                   @media (max-width: 500px) { span { color: red } }\n";
        let archive = styled_archive("stores_styles", css);
        let out = out_path("stores_styles");
        let guard = TempDbGuard(out.clone());
        let lines = std::cell::RefCell::new(Vec::new());
        let counts = build(std::slice::from_ref(&archive), &[], &out, &|line| {
            lines.borrow_mut().push(line.to_string());
        })
        .expect("the styled fixture builds");

        let conn = Connection::open(&out).unwrap();
        let stored: String = conn
            .query_row("SELECT css FROM dict_style WHERE dict_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(css, stored, "the text is stored verbatim");

        assert_eq!(1, counts.styles.sheets);
        assert_eq!(css.len(), counts.styles.bytes);
        assert_eq!(1, counts.styles.kept, "the fbox rule");
        assert_eq!(2, counts.styles.dropped, "a chrome class, and an at-rule body");
        assert_eq!(1, counts.styles.selectors);
        assert_eq!(0, counts.styles.malformed);
        let emitted = lines.into_inner();
        assert!(
            emitted
                .iter()
                .any(|l| l.starts_with("styles") && l.contains("Styled") && l.contains("2 dropped")),
            "the per-dictionary line reports the gap: {emitted:?}",
        );
        drop(guard);
        let _ = std::fs::remove_file(&archive);
    }

    /// The report includes property gaps as well as grammar gaps. A rule can pass
    /// selector grammar but use properties that this renderer cannot map to its
    /// box model.
    /// `tools/dict-census` counts declarations separately, so a report with only
    /// kept and dropped *rules* can hide a difference between the two counts.
    ///
    /// The test asserts expanded counts. `padding` is one authored declaration but
    /// four compiled longhands. `display` and `line-height` are outside
    /// `sheet::css_key`, and the `var()` border width names a custom property from
    /// Yomitan's popup chrome.
    #[test]
    fn a_build_reports_the_declarations_it_could_not_map() {
        let css = "span[data-sc-fbox] { padding: 0.1em; display: grid; line-height: 1.4;\n\
                   border-width: var(--gap) }\n";
        let archive = styled_archive("declaration_gap", css);
        let out = out_path("declaration_gap");
        let guard = TempDbGuard(out.clone());
        let lines = std::cell::RefCell::new(Vec::new());
        let counts = build(std::slice::from_ref(&archive), &[], &out, &|line| {
            lines.borrow_mut().push(line.to_string());
        })
        .expect("the styled fixture builds");

        assert_eq!(1, counts.styles.kept, "the selector compiles, so the rule stays");
        assert_eq!(0, counts.styles.dropped, "no grammar gap here");
        assert_eq!(4, counts.styles.declarations, "one padding, four longhands");
        assert_eq!(3, counts.styles.unmapped, "display, line-height, and the var()");
        let emitted = lines.into_inner();
        assert!(
            emitted.iter().any(|l| {
                l.starts_with("styles") && l.contains("4 declarations (3 unmapped)")
            }),
            "the per-dictionary line reports the property gap: {emitted:?}",
        );
        drop(guard);
        let _ = std::fs::remove_file(&archive);
    }

    /// An archive without a stylesheet writes no row and reports nothing.
    /// A build over that archive remains otherwise unchanged.
    #[test]
    fn an_archive_without_a_stylesheet_writes_no_row() {
        let (conn, _guard) = build_fixture_db("no_stylesheet_row");
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM dict_style", [], |r| r.get(0)).unwrap();
        assert_eq!(0, rows);
    }

    /// The build stores and counts a malformed stylesheet and still succeeds.
    /// The scanner keeps every rule that it recovers.
    #[test]
    fn a_malformed_stylesheet_still_builds() {
        let archive = styled_archive(
            "malformed_styles",
            "span[data-sc-fbox] { padding: 0.1em } div { color: red /* unterminated",
        );
        let out = out_path("malformed_styles");
        let guard = TempDbGuard(out.clone());
        let counts = build(std::slice::from_ref(&archive), &[], &out, &|_| {})
            .expect("a broken stylesheet must not fail a build");
        assert_eq!(1, counts.styles.sheets);
        assert_eq!(1, counts.styles.kept, "the rule before the break survives");
        assert_eq!(1, counts.styles.malformed);
        drop(guard);
        let _ = std::fs::remove_file(&archive);
    }

    // ---- Pitch ----

    /// Returns every accent that a Pitch Dictionary supplied, in table form.
    fn stored_pitch(conn: &Connection, term: &str, reading: &str) -> Vec<(i64, Option<i64>)> {
        let mut stmt = conn
            .prepare(
                "SELECT dict_id, downstep FROM pitch \
                 WHERE term = ?1 AND reading = ?2 ORDER BY rowid",
            )
            .unwrap();
        stmt.query_map(params![term, reading], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    /// This predicate reads bank content, not archive file names.
    /// That choice matters because one of the six archives named `[Pitch]` in the
    /// census has no `term_meta_bank_` at all.
    #[test]
    fn a_pitch_only_archive_supplies_the_pitch_role_and_a_term_archive_does_not() {
        assert!(pitch::supplies_pitch(&fixture("pitch.zip")));
        assert!(!pitch::supplies_pitch(&fixture("terms.zip")));
        assert!(!pitch::supplies_pitch(&fixture("freq.zip")), "a freq row is the other role");
    }

    /// A Pitch-only archive has no terms role. Its build adds a Dictionary row and
    /// its accents, but it adds no `entry`.
    #[test]
    fn a_pitch_only_archive_contributes_accents_and_no_entries() {
        let out = out_path("pitch_only");
        let _guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip"), fixture("pitch.zip")], &[], &out, &|_| {}).unwrap();
        let conn = Connection::open(&out).unwrap();

        let pitch_dict: i64 = conn
            .query_row("SELECT dict_id FROM dict WHERE name = 'FixturePitch'", [], |r| r.get(0))
            .expect("the pitch archive owns a dictionary row");
        let entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entry WHERE dict_id = ?1", [pitch_dict], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(0, entries, "no term bank, so no entry");
        assert_eq!(vec![(pitch_dict, Some(1)), (pitch_dict, Some(0))],
            stored_pitch(&conn, "猫", "ねこ"),
            "the archive's two rows for one reading merged, in arrival order");
    }

    /// One archive can supply both roles. The census has 9 frequency-only and
    /// 5 Pitch-only archives, but none with both roles.
    /// Neither role suppresses the other. Both use one Dictionary row.
    #[test]
    fn an_archive_supplying_terms_and_pitch_contributes_both() {
        let out = out_path("both_roles");
        let _guard = TempDbGuard(out.clone());
        build(&[fixture("both.zip")], &[], &out, &|_| {}).unwrap();
        let conn = Connection::open(&out).unwrap();

        assert!(pitch::supplies_pitch(&fixture("both.zip")));
        let dict_id: i64 = conn
            .query_row("SELECT dict_id FROM dict WHERE name = 'FixtureBoth'", [], |r| r.get(0))
            .unwrap();
        let entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entry WHERE dict_id = ?1", [dict_id], |r| r.get(0))
            .unwrap();
        assert_eq!(1, entries, "the term bank still builds");
        assert_eq!(vec![(dict_id, Some(2))], stored_pitch(&conn, "犬", "いぬ"));
    }

    /// The test stores every field that the schema permits, and the two fields that
    /// the builder does not draw. If the build dropped them, later code could not
    /// draw marks without a second schema bump.
    #[test]
    fn the_stored_row_carries_the_nasal_and_devoice_markers() {
        let out = out_path("pitch_markers");
        let _guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip"), fixture("pitch.zip")], &[], &out, &|_| {}).unwrap();
        let conn = Connection::open(&out).unwrap();

        let nasal: String = conn
            .query_row(
                "SELECT nasal FROM pitch WHERE term = '合鍵' AND reading = 'あいかぎ'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!("[4]", nasal);
        let devoice: String = conn
            .query_row(
                "SELECT devoice FROM pitch WHERE term = 'アーク灯' AND reading = 'アークとう'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!("[3]", devoice);
    }

    /// A row that repeats an accent stores it once. 大辞泉 has 11 such rows in the
    /// census.
    #[test]
    fn a_row_repeating_an_accent_stores_it_once() {
        let out = out_path("pitch_repeat");
        let _guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip"), fixture("pitch.zip")], &[], &out, &|_| {}).unwrap();
        let conn = Connection::open(&out).unwrap();

        let stored = stored_pitch(&conn, "一体", "いったい");
        assert_eq!(
            vec![Some(0), Some(1)],
            stored.into_iter().map(|(_, d)| d).collect::<Vec<_>>()
        );
    }

    /// The fixture reproduces a census failure. Each of five real Pitch archives
    /// stores CRC-32 values that do not match its payload.
    /// The old reader rejected all five, but Yomitan imported them.
    #[test]
    fn a_bank_whose_stored_checksum_is_wrong_still_reads() {
        let table = pitch::load_pitch(&fixture("badcrc.zip"))
            .expect("a wrong stored CRC-32 must not refuse an archive Yomitan accepts");

        assert_eq!(
            1,
            table.len(),
            "the payload is intact; only the checksum was ever wrong"
        );
        assert!(pitch::supplies_pitch(&fixture("badcrc.zip")));
    }


    /// Tests the complete path: a user installs two Pitch Dictionaries, hovers a
    /// word, and the card header shows their accents.
    /// The test builds a database, opens it, looks up a word, and presents a card.
    /// It proves that storage, reads, and reduction work together.
    #[test]
    fn a_hover_on_a_built_library_carries_the_accents_its_dictionaries_gave() {
        use crate::lookup::deconj::Deconjugator;
        use crate::lookup::engine::LookupEngine;
        use crate::lookup::model::Dictionary as _;
        use crate::lookup::sqlite::SqliteDictionary;
        use crate::present;

        let out = out_path("pitch_end_to_end");
        let _guard = TempDbGuard(out.clone());
        build(
            &[fixture("terms.zip"), fixture("pitch.zip"), fixture("pitch2.zip")],
            &[],
            &out,
            &|_| {},
        )
        .unwrap();

        let dict = SqliteDictionary::open(&out).expect("the built database opens");
        let installed = dict.dicts().unwrap();
        // A default `Config` names no Dictionary, so it enables every installed
        // Dictionary. This fresh-install behavior gives the card all three archives
        // in library order.
        let cfg = crate::config::Config::default().present_config(&installed);
        let card = |text: &str| {
            let hits =
                LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap();
            present::build(&hits, &dict.dicts().unwrap(), &cfg, &dict)
                .top
                .unwrap_or_else(|| panic!("no card for {text}"))
        };

        // `猫` and `ねこ` show the complete card path. `pitch.zip` names the pair
        // twice, with atamadaka in one row and heiban in another. The parser merges
        // those rows. `pitch2.zip` names only atamadaka.
        // The header therefore shows two rows: one shared accent with both
        // Dictionary names, and one accent that only the first Dictionary supplies.
        let neko = card("猫");
        assert_eq!(
            vec![
                crate::dict::pitch::Position::Downstep(1),
                crate::dict::pitch::Position::Downstep(0)
            ],
            neko.pitch.iter().map(|r| r.accent.position.clone()).collect::<Vec<_>>(),
            "{:?}",
            neko.pitch
        );
        assert_eq!(
            vec!["FixturePitch".to_string(), "FixturePitchTwo".to_string()],
            neko.pitch[0].dicts,
            "identical accents deduplicated, both names against the one row"
        );
        assert_eq!(
            vec!["FixturePitch".to_string()],
            neko.pitch[1].dicts,
            "and the accent only one of them gave names only that one"
        );

        // The two Pitch Dictionaries disagree about the `食べる` reading `たべる`, so
        // the card shows two rows.
        let taberu = card("食べる");
        assert_eq!(
            vec![
                crate::dict::pitch::Position::Downstep(2),
                crate::dict::pitch::Position::Downstep(0)
            ],
            taberu.pitch.iter().map(|r| r.accent.position.clone()).collect::<Vec<_>>(),
            "a disagreement is visible rather than hidden: {:?}",
            taberu.pitch
        );

        // The Anki note carries the same accents that the header shows.
        let fields = crate::anki::fields_from_card(&neko, &neko.blocks, true);
        let html = fields.get("pitch_html").expect("an HTML pitch field");
        assert!(html.contains("border-top"), "{html}");
        assert!(html.contains("FixturePitch \u{b7} FixturePitchTwo"), "{html}");
    }
}
