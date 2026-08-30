//! The schema and the writer.

use crate::dict::archive::{
    for_each_freq_row, for_each_media, for_each_term, read_index, read_styles_css,
};
use crate::dict::frequency::{lookup_freq, merge_freq_row, FreqTable};
use crate::dict::gloss::{renders_text, GlossDoc, Kind, NodeId};
use crate::dict::media::{self, Intrinsic};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Bumped to 3 by ticket 02: the `entry` record now stores the dictionary's
/// own structured-content glossary in place of two flattened vecs, and
/// ticket 03's media table lands under the same bump. Costs every user one
/// rebuild, once - a rebuild, not a re-import, because the library directory
/// keeps the archives and the rebuild flow replays them.
const SCHEMA_VERSION: i64 = 3;
#[cfg(test)]
const BATCH_ROWS: usize = 2;
#[cfg(not(test))]
const BATCH_ROWS: usize = 500;

/// One buffered `term` row.
#[allow(clippy::type_complexity)]
type TermBatchRow = (String, Option<String>, String, String, Option<i64>, i64, i64);

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
";

const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_term_surface ON term(surface);
CREATE INDEX IF NOT EXISTS idx_term_entry_id ON term(entry_id);
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
        self.malformed += other.malformed;
    }

    /// The one-line diagnostic, in the progress stream's own shape.
    fn line(&self, what: &str) -> String {
        let kib = self.bytes.div_ceil(1024);
        format!(
            "styles    {what}: {} sheets, {kib} KiB, {} rules kept, \
             {} dropped, {} selectors; {} malformed",
            self.sheets, self.kept, self.dropped, self.selectors, self.malformed,
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
    let tmp = building_path(out);
    if tmp.exists() {
        std::fs::remove_file(&tmp).with_context(|| format!("removing {}", tmp.display()))?;
    }

    let counts = build_into(terms, freqs, &tmp, on_progress).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    std::fs::rename(&tmp, out)
        .with_context(|| format!("replacing {} with {}", out.display(), tmp.display()))?;
    Ok(counts)
}

/// `out` with a .building suffix.
fn building_path(out: &Path) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(".building");
    PathBuf::from(name)
}

fn build_into(
    terms: &[PathBuf],
    freqs: &[PathBuf],
    out: &Path,
    on_progress: &dyn Fn(&str),
) -> Result<BuildCounts> {
    let freq_table = load_freqs(freqs)?;

    let mut conn = Connection::open(out).with_context(|| format!("creating {}", out.display()))?;
    conn.execute_batch(
        "PRAGMA page_size = 8192;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = OFF;",
    )?;
    create_schema(&conn)?;

    let mut entries: i64 = 0;
    let mut term_rows: i64 = 0;
    let mut media = MediaCounts::default();
    let mut styles = StyleCounts::default();
    let mut batches = Batches::new();

    let tx = conn.transaction()?;
    for (i, archive) in terms.iter().enumerate() {
        let slot =
            Slot { dict_id: i as i64 + 1, priority: i as i64, first_entry_id: entries + 1 };
        let one = insert_archive(&tx, archive, &slot, &freq_table, &mut batches, on_progress)?;
        entries += one.entries;
        term_rows += one.terms;
        media.add(one.media);
        styles.add(one.styles);
    }
    batches.flush(&tx)?;

    write_meta(&tx, terms, freqs)?;
    on_progress("building  creating index");
    ensure_indexes(&tx)?;
    tx.commit()?;
    conn.execute_batch("ANALYZE;")?;

    if media.referenced > 0 {
        on_progress(&media.line("all dictionaries"));
    }
    if styles.sheets > 0 {
        on_progress(&styles.line("all dictionaries"));
    }
    Ok(BuildCounts { entries, terms: term_rows, media, styles })
}

/// Merges the freq archives.
pub fn load_freqs(freqs: &[PathBuf]) -> Result<FreqTable> {
    let mut table = FreqTable::new();
    for fa in freqs {
        // Per archive, then overwrite.
        let mut one = FreqTable::new();
        for_each_freq_row(fa, |row| {
            merge_freq_row(&mut one, &row);
            Ok(())
        })?;
        table.extend(one);
    }
    Ok(table)
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

/// Buffers shared by archives.
pub(crate) struct Batches {
    json_buf: Vec<u8>,
    entries: Vec<(i64, i64, String)>,
    terms: Vec<TermBatchRow>,
    /// Content hash to `media_blob.blob_id`, across every archive in one
    /// build, so an asset shared by two dictionaries is stored once and
    /// costs one `SELECT` the second time instead of one per path.
    blobs: HashMap<[u8; 32], i64>,
}

impl Batches {
    pub(crate) fn new() -> Batches {
        Batches {
            json_buf: Vec::with_capacity(512),
            entries: Vec::with_capacity(BATCH_ROWS),
            terms: Vec::with_capacity(BATCH_ROWS),
            blobs: HashMap::new(),
        }
    }

    /// Writes what is buffered.
    pub(crate) fn flush(&mut self, tx: &rusqlite::Transaction) -> Result<()> {
        flush_entries(tx, &mut self.entries)?;
        flush_terms(tx, &mut self.terms)
    }
}

/// One archive into one slot.
pub(crate) fn insert_archive(
    tx: &rusqlite::Transaction,
    archive: &Path,
    slot: &Slot,
    freqs: &FreqTable,
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

    for_each_term(archive, |t| {
        // Serialise first, then parse the stored text: the record is what a
        // hover will read, so a term row that renders nothing here renders
        // nothing there either, and the emptiness test cannot drift from it.
        batches.json_buf.clear();
        serde_json::to_writer(&mut batches.json_buf, &t.glossary)?;
        let glossary = std::str::from_utf8(&batches.json_buf)
            .context("json output was not utf-8")?
            .to_string();
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
        collect_assets(&doc, &mut assets);
        entry_id += 1;
        if entry_id % 5000 == 0 {
            on_progress(&format!("progress  {entry_id} / ?"));
        }

        batches.entries.push((entry_id, dict_id, glossary));
        if batches.entries.len() >= BATCH_ROWS {
            flush_entries(tx, &mut batches.entries)?;
        }

        let written: &str = &t.term;
        let reading: &str = if t.reading.is_empty() { &t.term } else { &t.reading };
        let rank = lookup_freq(freqs, written, Some(reading));
        let same = written == reading;

        batches.terms.push((
            reading.to_string(),
            if same { None } else { Some(written.to_string()) },
            reading.to_string(),
            t.rules.clone(),
            rank,
            entry_id,
            dict_id,
        ));
        term_rows += 1;

        if !same {
            batches.terms.push((
                written.to_string(),
                Some(written.to_string()),
                reading.to_string(),
                t.rules.clone(),
                rank,
                entry_id,
                dict_id,
            ));
            term_rows += 1;
        }

        if batches.terms.len() >= BATCH_ROWS {
            // Flush entries first.
            flush_entries(tx, &mut batches.entries)?;
            flush_terms(tx, &mut batches.terms)?;
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

/// Flushes buffered entry rows.
fn flush_entries(tx: &rusqlite::Transaction, batch: &mut Vec<(i64, i64, String)>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (0..batch.len())
        .map(|i| {
            let b = i * 3 + 1;
            format!("(?{b},?{},?{})", b + 1, b + 2)
        })
        .collect();
    let sql = format!(
        "INSERT INTO entry (entry_id, dict_id, glossary) VALUES {}",
        placeholders.join(","),
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut idx = 1;
    for row in batch.iter() {
        stmt.raw_bind_parameter(idx, row.0)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, row.1)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, &row.2)?;
        idx += 1;
    }
    stmt.raw_execute()?;
    batch.clear();
    Ok(())
}

/// Flushes buffered term rows.
fn flush_terms(tx: &rusqlite::Transaction, batch: &mut Vec<TermBatchRow>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let placeholders: Vec<String> = (0..batch.len())
        .map(|i| {
            let b = i * 7 + 1;
            format!(
                "(?{b},?{},?{},?{},?{},?{},?{})",
                b + 1,
                b + 2,
                b + 3,
                b + 4,
                b + 5,
                b + 6,
            )
        })
        .collect();
    let sql = format!(
        "INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id) VALUES {}",
        placeholders.join(","),
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut idx = 1;
    for row in batch.iter() {
        stmt.raw_bind_parameter(idx, &row.0)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, &row.1)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, &row.2)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, &row.3)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, row.4)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, row.5)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, row.6)?;
        idx += 1;
    }
    stmt.raw_execute()?;
    batch.clear();
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

/// SHA-256 of a file's bytes.
fn hash_file(path: &Path) -> Result<String> {
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
        assert!(!building_path(&out).exists(), "no .building left behind");
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
        assert!(!building_path(&guard.0).exists());
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
        assert_eq!(vec![1, 2], ints(&conn, "SELECT dict_id FROM dict ORDER BY dict_id"));
        assert_eq!(vec![0, 1], ints(&conn, "SELECT priority FROM dict ORDER BY dict_id"));
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
}
