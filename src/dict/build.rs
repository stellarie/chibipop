//! The schema and the writer.

use crate::dict::archive::{for_each_freq_row, for_each_term, read_index};
use crate::dict::frequency::{lookup_freq, merge_freq_row, FreqTable};
use crate::dict::glossary::{extract_pos, flatten_glossary, render_glossary_html};
use crate::lookup::model::Sense;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: i64 = 2;
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
    senses   TEXT NOT NULL
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
";

const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_term_surface ON term(surface);
CREATE INDEX IF NOT EXISTS idx_term_entry_id ON term(entry_id);
";

/// Row counts a build wrote.
pub struct BuildCounts {
    pub entries: i64,
    pub terms: i64,
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
    let mut batches = Batches::new();

    let tx = conn.transaction()?;
    for (i, archive) in terms.iter().enumerate() {
        let slot =
            Slot { dict_id: i as i64 + 1, priority: i as i64, first_entry_id: entries + 1 };
        let one = insert_archive(&tx, archive, &slot, &freq_table, &mut batches, on_progress)?;
        entries += one.entries;
        term_rows += one.terms;
    }
    batches.flush(&tx)?;

    write_meta(&tx, terms, freqs)?;
    on_progress("building  creating index");
    ensure_indexes(&tx)?;
    tx.commit()?;
    conn.execute_batch("ANALYZE;")?;

    Ok(BuildCounts { entries, terms: term_rows })
}

/// Merges the freq archives.
pub(crate) fn load_freqs(freqs: &[PathBuf]) -> Result<FreqTable> {
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
}

/// Buffers shared by archives.
pub(crate) struct Batches {
    json_buf: Vec<u8>,
    entries: Vec<(i64, i64, String)>,
    terms: Vec<TermBatchRow>,
}

impl Batches {
    pub(crate) fn new() -> Batches {
        Batches {
            json_buf: Vec::with_capacity(512),
            entries: Vec::with_capacity(BATCH_ROWS),
            terms: Vec::with_capacity(BATCH_ROWS),
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

    let mut entry_id = slot.first_entry_id - 1;
    let mut term_rows: i64 = 0;

    for_each_term(archive, |t| {
        let glosses = flatten_glossary(&t.glossary);
        if glosses.is_empty() {
            return Ok(());
        }
        entry_id += 1;
        if entry_id % 5000 == 0 {
            on_progress(&format!("progress  {entry_id} / ?"));
        }
        let sense = Sense {
            glosses,
            glosses_html: render_glossary_html(&t.glossary),
            pos: extract_pos(&t.glossary),
            misc: Vec::new(),
        };
        batches.json_buf.clear();
        {
            let mut ser = serde_json::Serializer::with_formatter(&mut batches.json_buf, PySpaced);
            [&sense].serialize(&mut ser)?;
        }
        let senses_json = std::str::from_utf8(&batches.json_buf)
            .context("json output was not utf-8")?
            .to_string();
        batches.entries.push((entry_id, dict_id, senses_json));
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

    Ok(Loaded { name, entries: entry_id + 1 - slot.first_entry_id, terms: term_rows })
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
        "INSERT INTO entry (entry_id, dict_id, senses) VALUES {}",
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
    use rusqlite::OpenFlags;
    use serde_json::json;

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

    fn senses_for(conn: &Connection, surface: &str) -> serde_json::Value {
        let row: String = conn
            .query_row(
                "SELECT senses FROM entry JOIN term USING(entry_id) WHERE surface = ?1",
                [surface],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&row).unwrap()
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

    #[test]
    fn structured_content_flattens_to_one_gloss() {
        let (conn, _guard) = build_fixture_db("structured_content_flattens_to_one_gloss");
        let senses = senses_for(&conn, "食べる");
        assert_eq!(json!(["to eat"]), senses[0]["glosses"]);
    }

    #[test]
    fn structured_content_also_gets_an_html_rendering() {
        let (conn, _guard) = build_fixture_db("structured_content_also_gets_an_html_rendering");
        let senses = senses_for(&conn, "食べる");
        assert_eq!(json!(["<span>to eat</span>"]), senses[0]["glosses_html"]);
    }

    #[test]
    fn a_plain_string_glossary_gets_a_matching_html_rendering() {
        let (conn, _guard) =
            build_fixture_db("a_plain_string_glossary_gets_a_matching_html_rendering");
        let senses = senses_for(&conn, "ねこ");
        assert_eq!(json!(["cat"]), senses[0]["glosses"]);
        assert_eq!(json!(["cat"]), senses[0]["glosses_html"]);
    }

    #[test]
    fn part_of_speech_spans_are_separated_from_glosses() {
        let (conn, _guard) = build_fixture_db("part_of_speech_spans_are_separated_from_glosses");
        let senses = senses_for(&conn, "食べる");
        assert_eq!(json!(["1-dan", "transitive"]), senses[0]["pos"]);
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
            cells(&conn, "SELECT senses FROM entry WHERE dict_id = 1 ORDER BY entry_id"),
            cells(&conn, "SELECT senses FROM entry WHERE dict_id = 2 ORDER BY entry_id"),
            "and to the same senses"
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
}
