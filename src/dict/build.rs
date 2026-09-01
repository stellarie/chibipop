//! The schema and the writer.

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

/// Bumped to 4 by the dictionary-roles work: `reported_freq` keeps each
/// frequency dictionary's own claims per dictionary instead of merging them
/// into one build-time global, and ticket 02's pitch table lands under the
/// same bump. Costs every user one rebuild, once - a rebuild, not a
/// re-import, because the library directory keeps the archives and the
/// rebuild flow replays them.
const SCHEMA_VERSION: i64 = 4;
#[cfg(test)]
const BATCH_ROWS: usize = 2;
#[cfg(not(test))]
const BATCH_ROWS: usize = 500;

/// One buffered `term` row, as spans into the bank being written.
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
    -- at all (ADR-0015).
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
    -- with 0 meaning heiban, which is what all 511 488 accents ticket 01
    -- censused are; `pattern` is the `^[HL]+$` level-per-mora form, which
    -- the schema permits and neither corpus uses. Two columns rather than
    -- one because the two forms share no indexing origin and the string can
    -- say things no integer can - several falls, or a word that neither
    -- falls nor starts low.
    downstep INTEGER,
    pattern  TEXT,
    -- The moras this accent marks nasal and devoiced, as JSON arrays of
    -- 1-based mora indices, and the accent's own tags. Stored and not
    -- drawn: ticket 06 draws the marks, 25.8% of NHK's rows carry one, and
    -- dropping them here is exactly what would have cost that ticket a
    -- second schema bump.
    nasal    TEXT NOT NULL,
    devoice  TEXT NOT NULL,
    tags     TEXT NOT NULL
    -- Indexed on (term, reading) (see `INDEXES`) and on nothing else: the
    -- only read is one probe per shown card, and both columns are always
    -- given. Nothing on the term path comes here at all - pitch is per
    -- reading, so it cannot ride on a `term` row and must not widen one
    -- (ADR-0014).
);
";

/// Every table `dict_id` keys, children before parents, and the one list
/// [`crate::dict::edit::remove_dictionary`] walks.
///
/// It lives here, next to [`DDL`], because the two have to be read
/// together. Ticket 17 added `dict_style` above and did not add it to the
/// removal, and no test noticed for a whole ticket, because every committed
/// fixture ships no `styles.css` and so leaves `dict_style` empty - a
/// removal against an empty table passes whatever it forgets. One list read
/// beside the schema it belongs to is the cheapest thing that cannot drift.
///
/// `dict` itself is deliberately absent: it is the parent row the removal
/// deletes *last*, and it is counted on its own.
///
/// The order is the foreign-key order, and it is the one thing here that
/// cannot be derived cheaply - `term` references `entry`, and every table
/// references `dict`, so children have to go first. The *membership* is
/// checked against the schema of the database in hand
/// ([`dict_keyed_tables`]), so a table added above and forgotten here
/// aborts a removal by name instead of orphaning a row.
pub const DICT_KEYED: [&str; 6] =
    ["term", "entry", "media", "dict_style", "reported_freq", "pitch"];

/// Every table in *this* database that carries a `dict_id` column.
///
/// Read out of the live schema rather than assumed, because the point of it
/// is to catch a schema this code has not been taught about. `dict` is in
/// the answer - its primary key is named `dict_id` - and the caller is what
/// knows to hold it back to the end.
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

/// Row counts a build wrote.
pub struct BuildCounts {
    pub entries: i64,
    pub terms: i64,
    pub media: MediaCounts,
    pub styles: StyleCounts,
}

/// What one build made of the dictionaries' own stylesheets.
///
/// The build stores the text and compiles it once, here, only to record
/// these numbers - it persists no compiled form, so first use still
/// compiles. `dropped` is the gauge `tools/dict-census` reports against the
/// live grammar: it is the count of rules whose selectors this build cannot
/// read, and it shrinks as the grammar grows.
///
/// `declarations` and `unmapped` are the second half of that gauge, one
/// axis in: the census counts a stylesheet's declarations too, so a build
/// that never stated its own property gap would let the two arithmetics
/// drift with nobody to notice. A rule can survive the grammar and still
/// draw nothing this renderer has - `display: grid` is the corpus's
/// commonest declaration - and that is a property gap, not a selector one.
#[derive(Clone, Copy, Default, Debug)]
pub struct StyleCounts {
    /// Dictionaries that shipped a `styles.css`.
    pub sheets: usize,
    /// Bytes of CSS stored.
    pub bytes: usize,
    /// Rules compiled into a rule table.
    pub kept: usize,
    /// Rules dropped whole: an unsupported selector, or an at-rule body.
    pub dropped: usize,
    /// Selectors compiled, after expanding selector lists and `&` nesting.
    pub selectors: usize,
    /// Declarations compiled onto a [`crate::dict::gloss::StyleKey`], after
    /// expanding every `margin` and `padding` shorthand into its four
    /// longhands. Only from rules that were kept: a rule whose every
    /// declaration is unmapped counts none here and is not `dropped`
    /// either, because it is a property gap rather than a grammar one.
    pub declarations: usize,
    /// Declarations this build cannot express: a property outside
    /// `sheet::css_key`'s table, or a `var()` value naming a custom
    /// property declared on Yomitan's popup chrome, which this renderer has
    /// no equivalent of.
    pub unmapped: usize,
    /// Stylesheets that did not scan cleanly. Every rule the scanner did
    /// recover is still compiled; this counts the sheets, not the losses.
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

/// What extracting one corpus's - or one archive's - media produced.
///
/// Every field is a diagnostic the acceptance asks for: how many assets the
/// image nodes named, how many resolved, how much the database grew, and
/// how many did not resolve. The two failure counts are separate because
/// they mean different things to a dictionary author: `missing` is an
/// archive that does not ship what its own nodes reference, `unreadable` is
/// a file that is there and corrupt.
#[derive(Clone, Copy, Default, Debug)]
pub struct MediaCounts {
    /// Distinct asset paths the kept rows' image nodes referenced.
    pub referenced: usize,
    /// Media rows written.
    pub stored: usize,
    /// Blobs newly contributed - the count after content deduplication.
    pub blobs: usize,
    /// Bytes those new blobs added to the database.
    pub bytes: u64,
    /// Referenced, and no usable bytes in the archive.
    pub missing: usize,
    /// Present, and with no readable intrinsic size.
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

/// json.dumps's separators.
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

/// Matches json.dumps spacing.
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

    // A fresh exe has no data/ yet.
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Never destroy out on failure.
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

/// `out` with `suffix` appended to the whole file name.
///
/// Appended, not swapped for the extension: `chibipop.sqlite-wal` is the
/// name SQLite itself derives, and `chibipop.sqlite.building` sorts beside
/// the file it will become.
fn suffixed(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Puts a finished build in place of the live database.
///
/// Three things happen here, and their order is the whole crash-safety
/// story:
///
/// 1. **The built file is read back.** [`build_into`] runs the bulk load
///    under `synchronous = OFF`, which is the right trade for a load that
///    writes into a throwaway file - but it means this is the only place a
///    bad build can still be caught. A `quick_check` here costs one pass
///    over the new pages and buys the guarantee that a database nobody can
///    read is thrown away rather than promoted over a working one and
///    discovered on the next hover.
/// 2. **The previous database's `-wal` and `-shm` go.** A write-ahead log's
///    sidecars are keyed to the database's *file name*, never to its inode,
///    and `rename` replaces only the main file. So a rename on its own
///    leaves the old database's log sitting beside the new database, under
///    the name the new one now answers to, and the next reader **recovers
///    that log into the new file**. That is not a theoretical hazard: it is
///    `database disk image is malformed`, and it is what a user reported.
///    The checkpoint before the removal is what makes the removal lossless.
/// 3. **The rename.** One instant, and the only one.
///
/// Every prefix of that sequence leaves a database a reader can open:
///
/// - died during 1: `out` is untouched and whole. The orphaned `.building`
///   is removed by the next build, which unlinks it before it starts.
/// - died during 2: `out` is still the *old* database, and the checkpoint
///   moved everything its log held into it before the log was unlinked - so
///   the old database alone is complete. Living readers are unaffected
///   either way: an unlink drops a name, never an open file.
/// - died during 3: impossible. A rename is atomic, and both sides of it
///   are a whole database with no sidecar to recover.
///
/// The order that is *not* safe is the obvious one - rename first, tidy up
/// after - because the window between those two steps is exactly the bug.
fn promote(tmp: &Path, out: &Path, on_progress: &dyn Fn(&str)) -> Result<()> {
    verify_built(tmp, on_progress)?;
    drain_wal(out);
    drop_sidecars(out)?;
    std::fs::rename(tmp, out)
        .with_context(|| format!("replacing {} with {}", out.display(), tmp.display()))
}

/// Reads the finished build back before anything is allowed to depend on it.
///
/// The checkpoint first, so `quick_check` reads the pages a fresh reader
/// would and the promoted file needs no log of its own. Then the `fsync`,
/// because `synchronous = OFF` means nothing so far has promised the bytes
/// left the page cache - and `quick_check` reads back *through* that cache,
/// so it would happily bless a file that is not on the platter. A rename is
/// only worth as much as the file it publishes.
fn verify_built(built: &Path, on_progress: &dyn Fn(&str)) -> Result<()> {
    on_progress("building  checking the new database");
    let conn = Connection::open(built)
        .with_context(|| format!("reopening {} to check it", built.display()))?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .with_context(|| format!("checkpointing {}", built.display()))?;
    // quick_check, not integrity_check: the same page-level structural
    // check without the index-against-table cross-check, which on a
    // multi-gigabyte corpus is the difference between seconds and minutes.
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
    // `write(true)`, not `File::open`: Windows' `FlushFileBuffers` needs a
    // handle with write access and answers a read-only one with
    // ERROR_ACCESS_DENIED, so a plain read handle turns every build on
    // Windows into "Access is denied. (os error 5)".
    File::options()
        .write(true)
        .open(built)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("flushing {} to disk", built.display()))?;
    Ok(())
}

/// Moves everything the live database's log holds into the database itself,
/// so unlinking the log loses nothing.
///
/// Best effort, and deliberately: a reader mid-transaction can refuse a
/// truncating checkpoint, a file too damaged to open has nothing worth
/// draining, and this database is about to be replaced whichever way it
/// goes. What the attempt buys is the crash window in [`promote`]'s step 2 -
/// when it succeeds, the old database on its own is complete, so dying
/// before the rename leaves a whole database rather than a torso.
fn drain_wal(out: &Path) {
    if !out.exists() {
        return;
    }
    if let Ok(conn) = Connection::open(out) {
        // Never waits. A rebuild happens while the daemon is serving
        // lookups, so a reader is *expected* to be holding a snapshot, and
        // the default five-second busy timeout would spend five seconds of
        // every rebuild waiting for a checkpoint whose failure costs
        // nothing. `PASSIVE` first because it copies back every frame no
        // reader still needs and cannot block at all; `TRUNCATE` then
        // finishes the job whenever the readers happen to be between
        // transactions.
        let _ = conn.execute_batch(
            "PRAGMA busy_timeout = 0;
             PRAGMA wal_checkpoint(PASSIVE);
             PRAGMA wal_checkpoint(TRUNCATE);",
        );
    }
}

/// Removes the sidecars the previous database left under this name.
///
/// A hard error, not a best effort: a sidecar that will not go is a sidecar
/// the next reader recovers into the new file, so refusing to promote leaves
/// the user a working dictionary instead of a malformed one. That is also
/// the honest answer on a platform where an open file cannot be unlinked.
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
    // Every frequency dictionary is enabled and in library order in a fresh
    // build - there is no disabled state to read and no user order yet - so
    // the reduction is over all of them under the default strategy. A user
    // who has chosen another one reindexes; that is what a reindex is for,
    // and it is seconds against this function's minutes.
    let strategy = RankingStrategy::default();
    let names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
    let tables: Vec<FreqTable> = sources.into_iter().map(|s| s.table).collect();
    let ranks = frequency::reduce(&tables, strategy);

    let mut conn = Connection::open(out).with_context(|| format!("creating {}", out.display()))?;
    // The bulk load's own settings, and only its own: this is a throwaway
    // file that `promote` reads back before anything is allowed to depend
    // on it, so durability here buys nothing a `quick_check` does not.
    //
    // `journal_mode = MEMORY` rather than `WAL`: a write-ahead log makes
    // every page of a half-gigabyte load hit the disk twice - once into the
    // log, once when the checkpoint copies it back - and the load has no
    // concurrent reader to serve. A rollback journal over a file that
    // starts empty has almost no original pages to save, and in memory it
    // has none. The finished file is stamped back into WAL below, because
    // *that* one is read while `edit::add_dictionary` writes to it.
    //
    // Nothing else: a bigger `cache_size` and a `temp_store = MEMORY` were
    // both measured on the jitendex import and both cost peak memory for no
    // time at all - a load that only ever appends has nothing to re-read,
    // and the index build's sorter spills to a file the OS keeps in its own
    // cache anyway. 128 MiB of page cache cost 150 MiB of RSS and 0.08s.
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
    // Each archive with the `dict_id` it was just given, so the pitch pass
    // below attaches an archive's accents to its own dictionary row and
    // never finds one by name: `dict.name` is a title and two editions can
    // share one.
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

    // The frequency dictionaries' rows. An archive supplying frequency data
    // *and* terms is in both lists and is one Dictionary, so its claims go
    // under the `dict_id` it was already given rather than under a second
    // row wearing the same name - a role set means one archive can arrive
    // twice here (ADR-0014). A frequency archive `terms` does not name gets
    // its own row, after the term dictionaries so that a term archive's
    // `dict_id` is still its position in `terms`. The reduction the claims
    // were just reduced by is recorded beside them, so a reader knows which
    // dictionaries the ranks in `term` actually came from.
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
    // The mode the promoted database is read in; see the pragma block above.
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    // `analysis_limit` caps how many index rows each statistic samples. The
    // planner's choices here turn on orders of magnitude - is `surface`
    // selective? - and a 400-row sample answers that as well as a full scan
    // of 660 000 term rows, in a fraction of the time.
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

/// Every archive's Pitch patterns, under the dictionary that gave them.
///
/// Over every archive the build read rather than over a list of its own,
/// because the pitch role is what an archive's `term_meta_bank_` rows hold
/// and not which list it arrived in: a pitch-only archive, one that also
/// carries terms, and one that also carries frequency data all store their
/// accents here, under the one `dict` row they already have. Which of them a
/// reader consults and in what order is the enabled pitch list's business,
/// which is config's and never a build input (ADR-0014) - the same split
/// `reported_freq` takes, where every dictionary's claims are stored and the
/// enabled list decides what they mean.
///
/// Returns the accents stored across every archive, for the progress line.
fn store_archive_pitch(
    tx: &rusqlite::Transaction,
    read: &[(i64, &Path)],
    on_progress: &dyn Fn(&str),
) -> Result<usize> {
    let mut total = 0;
    for &(dict_id, archive) in read {
        // The load first and the title second: an archive with no
        // term-meta bank answers from its central directory, and asking
        // for its title would cost a second read of its `index.json` for
        // nothing.
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

/// Stores one dictionary's Pitch patterns, one row per accent.
///
/// One row per accent rather than one per reading with a list in it: the
/// accents of one reading are what a card header draws as its own rows, and
/// a reader that had to split a packed column could not have used the index
/// to find them.
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

/// One accent's mora markers or tags, as the JSON array the column holds.
///
/// JSON rather than a separated string because a tag is an author's own text
/// and could hold any separator; one encoding for all three columns is one
/// reader and one writer rather than three.
fn to_json_list<T: Serialize>(list: &[T]) -> Result<String> {
    serde_json::to_string(list).context("encoding an accent's mora list")
}

/// Each frequency archive's own claims, in the order they were given.
///
/// Per archive and no further. `merge_freq_row`'s lowest-rank-wins rule is
/// right *within* one archive and stays; across archives there is no rule to
/// apply until a ranking strategy says which one, so this used to end in
/// `table.extend(one)` and the last archive read silently won every key it
/// named.
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

/// A slice of a [`PreparedBank`]'s buffer: `(start, end)` in bytes.
///
/// A pair of `u32` rather than a `String` per field, because a bank holds
/// tens of thousands of rows and every one of them would otherwise be four
/// heap allocations the binder immediately copies out of. The buffer they
/// index is at most one bank's text ([`crate::dict::archive`]'s `MAX_BANK`,
/// 256 MiB) because every byte pushed into it is a byte the bank already
/// spelled out, so `u32` cannot overflow.
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

/// One bank's kept rows, and the assets they named.
///
/// Everything a bank contributes lands in one growable buffer, so handing a
/// bank from the thread that parsed it to the thread that writes it moves
/// three allocations rather than a hundred thousand.
struct PreparedBank {
    text: String,
    rows: Vec<PreparedRow>,
    assets: BTreeSet<String>,
}

/// How many threads an import parses banks on.
///
/// Capped, and not by politeness: each thread holds the bank it is reading
/// and the bank it has prepared, so the ceiling is what bounds a rebuild's
/// peak memory at roughly `2 * MAX_IMPORT_THREADS` bank-sized buffers. Eight
/// is past the knee on every corpus measured - the writer thread's SQLite
/// inserts are the floor from about four onwards.
const MAX_IMPORT_THREADS: usize = 8;

fn worker_count(banks: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    banks.min(cores).clamp(1, MAX_IMPORT_THREADS)
}

/// Buffers shared by archives.
pub(crate) struct Batches {
    /// Row buffers, reused for every bank of every archive. They hold spans
    /// into the bank currently being written, so they are always empty
    /// between banks.
    entries: Vec<(i64, i64, Span)>,
    terms: Vec<TermBatchRow>,
    /// The full-batch `INSERT`s, built once. A 500-row insert names 3 500
    /// placeholders, and a full batch is what almost every one of an
    /// import's thousands of flushes carries. The odd short batch - a
    /// bank's last rows, or a term batch that overshot by one because an
    /// entry contributed two rows - builds its own.
    entry_sql: String,
    term_sql: String,
    /// Content hash to `media_blob.blob_id`, across every archive in one
    /// build, so an asset shared by two dictionaries is stored once and
    /// costs one `SELECT` the second time instead of one per path.
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

/// One archive into one slot.
///
/// `ranks` is the reduced Frequency rank per headword - every enabled
/// frequency dictionary's claim already put through the ranking strategy
/// ([`frequency::reduce`]) - because the strategy is applied when
/// `term.freq` is written and never when it is read.
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

/// One prepared bank into `entry` and `term`.
///
/// The whole bank is written before the next one is touched, so the row
/// buffers only ever hold spans into `bank.text`.
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
            // Entries first: a `term` row names the `entry` row it belongs to.
            flush_entries(tx, &bank.text, batches)?;
            flush_terms(tx, &bank.text, batches)?;
        }
    }
    flush_entries(tx, &bank.text, batches)?;
    flush_terms(tx, &bank.text, batches)
}

/// Every term bank of one archive, prepared off the writer's thread and
/// handed over in archive order.
///
/// Order is not an aesthetic: `entry_id` is assigned as banks arrive, and a
/// rebuild that numbered its entries by whichever thread finished first
/// would write a different database every time. So the workers race and the
/// results are re-sequenced here, which costs at most one bank of latency
/// per out-of-order finish.
///
/// A bank is the unit of work because it is self-contained JSON: parsing
/// one, parsing each of its glossaries, and testing them for renderable
/// text is about four fifths of an import's CPU and shares nothing between
/// banks. Only the SQLite writes have to be serial, and they are.
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
    // A rendezvous channel, so a worker that finishes early blocks instead
    // of running ahead and buffering banks nobody has asked for yet. That,
    // plus the reorder buffer below, is what bounds peak memory.
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
                    // A closed channel means the writer has given up.
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

/// One bank's text into the rows the writer binds.
///
/// The whole per-row cost of an import lives here: the row parse, the
/// glossary parse, and the emptiness test. Nothing touches the database.
fn prepare_bank(text: &str, name: &str, ranks: &FreqTable) -> Result<PreparedBank> {
    let mut bank =
        PreparedBank { text: String::with_capacity(text.len()), rows: Vec::new(), assets: BTreeSet::new() };
    let mut glossary = String::new();

    for_each_row(text, name, |t| {
        // Minify first, then parse the stored text: the record is what a
        // hover will read, so a term row that renders nothing here renders
        // nothing there either, and the emptiness test cannot drift from it.
        glossary.clear();
        minify_json(t.glossary, &mut glossary);
        // An image-only or whitespace-only glossary is not an entry. Same
        // rule as before the tree existed; it is what keeps a gaiji-only
        // term row out of the term index.
        let doc = GlossDoc::parse(&glossary);
        if !renders_text(&doc) {
            return Ok(());
        }
        // Only a kept row's images are collected, and from the parse the
        // emptiness test already ran: a row that renders no text is not an
        // entry, so no hover can reach it and its assets are unreachable
        // too. Walking the tree here also means the build never re-scans
        // the raw JSON for paths.
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

/// The archive's own glossary bytes, minus the whitespace between them.
///
/// Stored verbatim otherwise, which is what the `entry.glossary` column has
/// always claimed to hold and what it now actually holds: the previous
/// writer round-tripped every glossary through a `serde_json::Value` and a
/// serializer, which cost two of every six seconds a jitendex import spent
/// and silently re-sorted every object's keys on the way through.
///
/// The whitespace does have to go: a pretty-printed bank is 9% larger than
/// its minified form on jitendex, and that is 9% of a half-gigabyte
/// database for bytes no reader ever looks at.
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
            // Runs are copied whole, so compact JSON - which is nearly all
            // of it - is one `push_str` of the entire glossary.
            into.push_str(&json[copied..i]);
            copied = i + 1;
        }
    }
    into.push_str(&json[copied..]);
}

/// One dictionary's own `styles.css`, stored verbatim and counted once.
///
/// Stored, not compiled: the row holds the text and the matcher compiles it
/// on first use of the dictionary, so a matcher fix ships as a patch exactly
/// as a parser fix does. The compile here throws its result away and exists
/// only to fill the build report - the dropped-rule count is the gauge the
/// census reports against, and a number nobody records is a number that
/// silently rots.
///
/// Total: an archive whose stylesheet cannot be read stores none and the
/// build carries on. A dictionary's boxes are worth less than its entries.
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

/// Every asset path this document's image nodes name.
///
/// A linear sweep of the arena rather than a tree walk: `all_nodes` is in
/// parse order and an image node is a leaf, so there is nothing the tree
/// shape would add. `Kind::Image` rather than `Tag::Img`, because a
/// `type: "image"` glossary item is an image with no tag at all.
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

/// Extracts one archive's referenced assets into the media store.
///
/// The contract a missing asset gets, and the one ticket 12's `alt`-text
/// ladder is written against: **a media row exists only when the bytes are
/// in the store and the intrinsic size is known.** An absent path and an
/// unsizeable file both produce no row and one diagnostic line, and never a
/// failed build - an archive is third-party bytes, and one corrupt gaiji
/// must not cost a user their whole rebuild.
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

/// How many named offenders a diagnostic lists before it stops.
///
/// A dictionary that ships none of its own assets would otherwise print one
/// line per node, and 字通 has 139 138 of them. The count is always
/// complete; the names are a sample.
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

/// The blob holding these bytes, inserting it the first time they are seen.
///
/// `INSERT OR IGNORE` and then a read, rather than a read and then an
/// insert: `edit::add_dictionary` writes into a live database whose blob
/// table this process did not fill, so an unseen hash can still already
/// have a row. The in-memory map in front of it is what keeps the whole
/// build to one statement pair per *distinct* blob.
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

/// `head` followed by `rows` tuples of `cols` numbered placeholders.
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

/// Creates any missing index.
///
/// Needs a writable connection.
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

/// Records provenance in meta.
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

/// SHA-256 of a file's bytes, lowercase hex.
///
/// The one hash in the crate: `meta.source_hashes` records it per archive,
/// and [`crate::library::Library`] uses it to tell two names for one
/// dictionary apart from two dictionaries. Streamed in 64 KB reads, which
/// on this implementation is about 400 MiB/s - the number the library's
/// gate on it is sized against.
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

    /// Feeds more bytes in.
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

    /// Ends the hash; returns it.
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

    /// The record as stored, and as a hover reads it.
    fn stored_glossary(conn: &Connection, surface: &str) -> String {
        conn.query_row(
            "SELECT glossary FROM entry JOIN term USING(entry_id) WHERE surface = ?1",
            [surface],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// The gloss tree a hover would parse out of the record.
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

    /// The record keeps the dictionary's own structured content, not a
    /// rendering of it: that is what makes a renderer or parser fix a patch
    /// instead of a rebuild.
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
        // A fresh exe has no data/.
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

    // ---- promoting a finished build over the live database ----

    /// The named dictionary in a database, which is how these tests tell the
    /// old file from the new one.
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

    /// A live database, grown past one page and then checkpointed, with a
    /// still-open write connection.
    ///
    /// Grown on purpose. That is what makes a stale-log failure the user's
    /// failure rather than a merely stale answer: a log whose pages are
    /// replayed into a file laid out differently leaves the header claiming
    /// what the file does not hold, which is `database disk image is
    /// malformed`. A single-page database has nothing to disagree about.
    fn live_with_a_log(out: &Path) -> Connection {
        build(&[fixture("terms.zip")], &[], out, &|_| {}).expect("the first build");
        let live = Connection::open(out).unwrap();
        live.execute_batch(
            "INSERT INTO meta (k, v)
               WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 4000)
               SELECT 'pad' || i, hex(zeroblob(100)) FROM s;
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT OR REPLACE INTO meta (k, v) VALUES ('marker', 'old');",
        )
        .unwrap();
        assert!(suffixed(out, "-wal").exists(), "the log is what this test is about");
        live
    }

    /// The same database held the way a running daemon holds it, and left in
    /// the only state a stale log can actually do harm from: a log whose
    /// frames are **not yet copied back** into the database.
    ///
    /// Every step of that is load-bearing, so none of it is decoration.
    /// The reader takes its snapshot *before* the last commit, so no close
    /// can copy that commit back. The reader is read-only, so it can neither
    /// drain the log nor delete it. Between them the log outlives the writer
    /// with live frames in it - which is exactly the state a daemon holding a
    /// dictionary leaves a rebuild to find, and the state in which replaying
    /// the log into a different file destroys it. A fully copied-back log is
    /// harmless: its index says there is nothing to replay, and a reader
    /// reads straight past it.
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

    /// The bug a user hit, and the reason [`drop_sidecars`] exists.
    ///
    /// A write-ahead log's sidecars are keyed to the database's **file
    /// name**, never to its inode, and `rename` replaces only the main
    /// file. So a promote that renames and stops leaves the old database's
    /// log sitting under the new database's name, and the next cold reader
    /// recovers it straight into the new file - writing the old
    /// dictionary's pages over the new one's, permanently, on disk. The
    /// reader does not get a stale answer, it gets
    /// `database disk image is malformed`.
    ///
    /// Without [`drop_sidecars`] this test reports
    /// `wrong # of entries in index sqlite_autoindex_meta_1` from
    /// `integrity_check` - the same class of answer as the
    /// `Tree 3 page 3 cell 0: invalid page number` the user's database gave.
    /// Which page-level complaint comes out depends on which of the new
    /// file's pages the old log happens to land on, so the assertion that
    /// matters is the invariant: **no sidecar of the replaced database may
    /// survive the promote.**
    #[test]
    fn a_promote_never_leaves_the_previous_databases_log_beside_the_new_one() {
        let out = out_path("stale_log");
        let guard = TempDbGuard(out.clone());
        let live = held_by_a_reader(&out);

        // A different dictionary built over it with `live` still open -
        // exactly as the daemon's connection is during a rebuild.
        build(&[media_archive()], &[], &out, &|_| {}).expect("the rebuild promotes");

        // The invariant, checked while the daemon's handle is still there.
        assert!(!suffixed(&out, "-wal").exists(), "the old log must not outlive the promote");
        assert!(!suffixed(&out, "-shm").exists(), "nor its index");

        // Then the symptom, met the way the user met it: the process that
        // held the old log has gone, and something opens the file cold.
        drop(live);
        assert_eq!("ok", verdict(&out), "a cold reader must find the promoted file sound");
        assert_eq!("FixtureMedia", first_dict(&out), "it is the dictionary just built");
        assert_eq!(None, marker(&out), "with nothing recovered out of the old log");
        drop(guard);
    }

    /// The crash window [`promote`]'s ordering exists to make safe. Between
    /// the old log going and the rename landing, the old database has to be
    /// one a reader can still open whole - which is what the checkpoint
    /// before the removal buys, and why the removal cannot come after the
    /// rename instead.
    #[test]
    fn a_promote_interrupted_before_the_rename_leaves_the_old_database_whole() {
        let out = out_path("interrupted");
        let guard = TempDbGuard(out.clone());
        let live = live_with_a_log(&out);
        let tmp = suffixed(&out, ".building");
        let tmp_guard = TempDbGuard(tmp.clone());
        build_into(&[media_archive()], &[], &tmp, &|_| {}).expect("the new build");

        verify_built(&tmp, &|_| {}).expect("the new build checks out");
        drain_wal(&out);
        drop_sidecars(&out).expect("the old log goes");
        // -- the process dies here, one syscall short of the rename --

        assert_eq!("ok", verdict(&out), "the old database has to still be readable");
        assert_eq!("FixtureTerms", first_dict(&out), "and still be the old one");
        assert_eq!(
            Some("old".to_string()),
            marker(&out),
            "the checkpoint is what keeps the log's own rows: dropping a log \
             that had not been drained is how a promote loses a transaction",
        );
        drop(live);
        drop(tmp_guard);
        drop(guard);
    }

    /// The promote gate. `build_into` runs the bulk load under
    /// `synchronous = OFF`, so a build killed mid-write can leave a file
    /// that opens and does not read - and promoting that over a working
    /// dictionary turns one lost rebuild into a lost dictionary. Driven
    /// through [`promote`] rather than [`build`] on purpose: a torn page is
    /// not something a successful build can be asked to produce.
    #[test]
    fn a_build_that_does_not_check_out_is_refused_rather_than_promoted() {
        let out = out_path("torn_build");
        let guard = TempDbGuard(out.clone());
        build(&[fixture("terms.zip")], &[fixture("freq.zip")], &out, &|_| {}).unwrap();
        let good = std::fs::read(&out).unwrap();

        // A finished build with a page zeroed out of the middle of it. Page
        // 1 is the header and would fail to open at all, which would prove
        // only that SQLite reads headers; this is the harder case, a file
        // that opens cleanly and is not a database.
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
        // Two term archives, then the frequency dictionary: a term archive's
        // `dict_id` is still its position in `terms`, and `freq.zip` follows
        // as a dictionary in its own right.
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

    /// The one thing threading a build can silently break.
    ///
    /// `banks.zip` has twelve term banks and one of them is a hundred times
    /// the size of the others, so eleven banks finish while bank 3 is still
    /// being parsed. Every one of those has to wait: `entry_id` is assigned
    /// in archive order, and a build that numbered by completion order
    /// would write a different database on every run and break every
    /// `entry_id` a caller already holds.
    ///
    /// Asserted as the whole sequence rather than a count, because a count
    /// passes whatever the order was.
    #[test]
    fn a_many_bank_archive_is_numbered_in_archive_order() {
        let out = out_path("many_banks");
        let _guard = TempDbGuard(out.clone());

        let counts = build(&[fixture("banks.zip")], &[], &out, &|_| {}).unwrap();

        // Eleven banks of four rows, and one of four hundred.
        assert_eq!(11 * 4 + 400, counts.entries);
        let conn = Connection::open(&out).unwrap();
        let mut stmt = conn.prepare("SELECT surface FROM term ORDER BY rowid").unwrap();
        let surfaces: Vec<String> =
            stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        // Two `term` rows per entry, reading first: the headwords here are
        // all kanji-shaped, so none of them collapses to one row.
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

    /// Two runs of a threaded build have to agree byte for byte in every
    /// table a reader touches. The timestamp in `meta` is the one thing that
    /// may differ, so it is excluded rather than the comparison weakened.
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

    /// The stored glossary is the archive's own bytes, minus the whitespace
    /// between them. Key order included: the writer used to round-trip every
    /// glossary through a `serde_json::Value`, whose map is sorted, and
    /// nothing about that was ever wanted.
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
        // Minifying must never change what the JSON means.
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

    /// Never the lookup connection.
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

    // ---- the media store (ticket 03) ----

    fn media_archive() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/media.zip")
    }

    /// The media fixture archive built by the real builder, plus the
    /// progress lines it emitted.
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

    /// Extracting every asset in an archive would store an image
    /// dictionary's whole glyph set: 30 of 52 structured-content
    /// dictionaries emit images, and only the paths their nodes name can
    /// ever be painted.
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
                // Sized from its header and undecodable behind it, which
                // is a real row and ticket 12's paint-time rung.
                "gaiji/torn.png",
                "gaiji/two.svg",
            ],
            media_paths(&conn),
        );
        // `unused.png` is in the archive and named by nothing.
        assert_eq!(8, counts.media.stored);
        assert_eq!(10, counts.media.referenced, "eight stored, one absent, one unsizeable");
    }

    /// A term row whose glossary renders no text is not an entry, so no
    /// hover can reach it - and neither can its images.
    #[test]
    fn an_image_only_term_row_contributes_no_media() {
        let (conn, _guard, ..) = build_media_db("media_image_only_row");
        assert!(
            !media_paths(&conn).iter().any(|p| p == "gaiji/dropped.png"),
            "the archive ships it and only a dropped row references it",
        );
    }

    /// 字通 averages more than four image nodes per term row over a few
    /// thousand distinct gaiji, so sharing bytes across paths is
    /// load-bearing and not an optimisation.
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

    /// The load-bearing column set. 99 807 census image nodes declare
    /// neither `width` nor `height`, so a wrong number here is a
    /// mis-measured line rather than a mis-drawn picture.
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
            // The logical screen of a two-frame animation, not its 4x3
            // frames: that is the canvas a browser lays out.
            ("gaiji/four.gif", "gif", 9.0, 5.0),
            ("gaiji/two.svg", "svg", 64.0, 32.0),
            // No width or height on the root element: the viewBox is the
            // size, which is the common shape for a gaiji SVG.
            ("gaiji/ratio.svg", "svg", 100.0, 40.0),
            // The one format whose size lives in an item property rather
            // than a header, and the one this build cannot rasterize -
            // which is exactly why the size has to be recorded here.
            ("gaiji/five.avif", "avif", 480.0, 120.0),
        ] {
            let (got_format, got_w, got_h, got_aspect) = recorded(path);
            assert_eq!(format, got_format, "{path}");
            assert_eq!((w, h), (got_w, got_h), "{path}");
            // In `f32`, because that is the type the row is written from
            // and read back into - the popup's geometry is `f32`
            // throughout, and `9 / 5` is not exact in either width.
            assert_eq!(
                w as f32 / h as f32,
                got_aspect as f32,
                "{path}: aspect is a column, not a derivation",
            );
        }
    }

    /// The contract ticket 12's `alt`-text ladder is written against: a
    /// media row exists only when the bytes are stored and the size is
    /// known, so a lookup that answers nothing means "fall back".
    #[test]
    fn a_missing_or_unreadable_asset_is_counted_and_never_fails_the_build() {
        let (conn, _guard, counts, lines) = build_media_db("media_absent");
        let paths = media_paths(&conn);
        assert!(!paths.iter().any(|p| p == "gaiji/missing.png"), "absent from the archive");
        assert!(!paths.iter().any(|p| p == "gaiji/broken.png"), "present and unsizeable");
        assert_eq!(1, counts.media.missing);
        assert_eq!(1, counts.media.unreadable);
        // And the entry that referenced them is still an entry, with its
        // own text intact.
        assert_eq!(vec!["fish".to_string()], plain_items(&stored_doc(&conn, "さかな")));

        // Each one is named, so a dictionary author can act on it.
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

    /// An archive with no image nodes must cost nothing: no second pass
    /// over the zip, no rows, and no diagnostic noise.
    #[test]
    fn a_dictionary_with_no_image_nodes_writes_no_media_and_says_nothing() {
        let (conn, _guard) = build_fixture_db("media_none");
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0)).unwrap();
        assert_eq!(0, rows);
    }

    // ---- a dictionary's own styles.css (ticket 17) ----

    /// One archive, written here, built by the real builder.
    ///
    /// `index.json` at the root because `read_index` requires it there, and
    /// the stylesheet one directory deep, which is the other of the two
    /// places the census found one.
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

    /// The build stores the text and reports what compiling it kept and
    /// dropped. Stores the *text*: nothing compiled is persisted, because the
    /// matcher runs on first use so that a matcher fix ships as a patch
    /// rather than as a rebuild. The compile here exists only to fill this
    /// report, and the dropped count is what `tools/dict-census` scores
    /// against.
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

    /// The property gap reaches the report, not just the grammar gap. A rule
    /// can pass the selector grammar whole and still declare things this
    /// renderer has no box model for, and `tools/dict-census` counts a
    /// stylesheet's declarations independently - so a build that recorded
    /// only its kept and dropped *rules* would let the two arithmetics drift
    /// unnoticed.
    ///
    /// The numbers are the expansion's, which is why they are asserted and
    /// not just non-zero: `padding` is one authored declaration and four
    /// compiled longhands, `display` and `line-height` are outside
    /// `sheet::css_key`'s table, and the `var()` border width names a custom
    /// property declared on Yomitan's chrome.
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

    /// A dictionary that ships none writes no row and reports nothing, and a
    /// build over such a corpus is unchanged.
    #[test]
    fn an_archive_without_a_stylesheet_writes_no_row() {
        let (conn, _guard) = build_fixture_db("no_stylesheet_row");
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM dict_style", [], |r| r.get(0)).unwrap();
        assert_eq!(0, rows);
    }

    /// Malformed CSS is stored, counted, and never fails the build: the
    /// scanner keeps every rule it did recover.
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

    // ---- pitch

    /// Every accent a pitch dictionary gave, as the table holds them.
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

    /// The predicate reads banks and not filenames, which is the whole
    /// reason it exists: one of the six archives named `[Pitch]` in ticket
    /// 01's census has no `term_meta_bank_` at all.
    #[test]
    fn a_pitch_only_archive_supplies_the_pitch_role_and_a_term_archive_does_not() {
        assert!(pitch::supplies_pitch(&fixture("pitch.zip")));
        assert!(!pitch::supplies_pitch(&fixture("terms.zip")));
        assert!(!pitch::supplies_pitch(&fixture("freq.zip")), "a freq row is the other role");
    }

    /// And a pitch-only archive supplies no terms role: its build
    /// contributes a dictionary row and its accents, and not one `entry`.
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

    /// Both roles from one archive, which ticket 01's census has no specimen
    /// of - 9 frequency-only and 5 pitch-only, none both. Neither role
    /// suppresses the other and both land under one dictionary row.
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

    /// Every field the schema permits reaches the table, including the two
    /// this ticket does not draw: dropping them would cost ticket 06 a second
    /// schema bump.
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

    /// One row listing an accent twice stores it once (大辞泉's `一体`, 11
    /// such rows in the census).
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

    /// The blocker ticket 01 measured, as a fixture: every one of the five
    /// real pitch archives stores CRC-32 values that do not match its own
    /// payload, and this reader used to refuse all of them while Yomitan
    /// imported them cleanly.
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


    /// End to end, which is the ticket's own goal: a user installs two pitch
    /// dictionaries, hovers a word, and the card header carries the accents -
    /// deduplicated where they agree, both rows where they do not.
    ///
    /// Build, open, look up, present. The one test that proves the storage,
    /// the read and the reduction are wired to each other rather than each
    /// correct alone.
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
        // A config naming nothing enables every dictionary it finds, which
        // is what a fresh install resolves to and what this end-to-end
        // wants: all three archives in library order.
        let cfg = crate::config::Config::default().present_config(&installed);
        let card = |text: &str| {
            let hits =
                LookupEngine::new(Deconjugator::new(Vec::new())).run(&dict, text).unwrap();
            present::build(&hits, &dict.dicts().unwrap(), &cfg, &dict)
                .top
                .unwrap_or_else(|| panic!("no card for {text}"))
        };

        // 猫 / ねこ is the whole story on one card. `pitch.zip` names it
        // twice - atamadaka in one row and heiban in another, which the
        // parser merged - and `pitch2.zip` names the atamadaka only. So the
        // header draws two rows: the shared accent naming both dictionaries,
        // and the one only the first gave naming one.
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

        // They disagree about 食べる / たべる, so it draws two rows.
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

        // And the mined note carries the same accents the header drew.
        let fields = crate::anki::fields_from_card(&neko, &neko.blocks);
        let html = fields.get("pitch_html").expect("an HTML pitch field");
        assert!(html.contains("border-top"), "{html}");
        assert!(html.contains("FixturePitch \u{b7} FixturePitchTwo"), "{html}");
    }
}
