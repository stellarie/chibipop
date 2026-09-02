//! This example shows where the 63 µs hover time goes.
//!
//! [docs/research/hover-parse-cost.md](../docs/research/hover-parse-cost.md)
//! measures one hover as one number: 63.3 µs at p50 for `terms_for` +
//! `entries` on a library with twelve Dictionaries.
//! It states that the query part is a *floor* because it times one point query,
//! not the deconjugation fan-out that a real hover issues.
//! This example splits that number into its parts.
//! It measures the two costs that an arena and bloom filter architecture can reduce:
//! negative lookups and conversion of each row into owned `TermRow`s`.
//!
//! ```sh
//! cargo run -p chibipop --release --example lookup_breakdown -- all [db] [label]
//! cargo run -p chibipop --release --example lookup_breakdown -- probe   [db] [label]
//! cargo run -p chibipop --release --example lookup_breakdown -- fanout  [db] [label]
//! cargo run -p chibipop --release --example lookup_breakdown -- entries [db] [label]
//! cargo run -p chibipop --release --example lookup_breakdown -- worst   [db] [label]
//! cargo run -p chibipop --release --example lookup_breakdown -- plan    [db]
//! cargo run -p chibipop --release --example lookup_breakdown -- bloom   [db] [label]
//! ```
//!
//! Every mode opens through `SqliteDictionary::open`.
//! It passes `SQLITE_OPEN_READ_ONLY` without `SQLITE_OPEN_CREATE` and runs no migration.
//! A second handle in this file times the column reads.
//! It uses the same flags and the same `mmap_size` pragma.
//! No mode can modify the live database.
//!
//! `tools/hover-parse-bench` extracted the sample once.
//! This example reads it from `results/hover-payloads.jsonl`.
//! Every number therefore matches `hover-parse-cost.md`.
//! Release mode is mandatory.

use anyhow::{Context, Result};
use chibipop::lookup::deconj::{Deconjugator, Form};
use chibipop::lookup::engine::{clean_input, LookupEngine, MAX_LOOKUP_CHARS, MAX_RESULTS};
use chibipop::dict::gloss::GlossDoc;
use chibipop::dict::pitch::PitchClaim;
use chibipop::lookup::model::{Dictionary, Entry, TermRow};
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// `SWEEPS` sets the number of full sweeps over the sample list.
/// The benchmark keeps each item's median sweep.
/// A list sweep keeps the access pattern close to a real hover sequence.
const SWEEPS: usize = 5;

/// The miss population is the *real* one.
/// It contains every surface that `LookupEngine::run` probes and that returns zero rows.
/// It has tens of thousands of distinct surfaces.
/// The benchmark reduces it to this size with a stride so the sweep stays quick.
const MAX_MISS_PROBES: usize = 3000;

/// `PROSE_CHARS` sets the prose tail length that each run stores.
const PROSE_CHARS: usize = 40;
/// `PROSE_POOL` sets the prose pool limit.
const PROSE_POOL: usize = 4000;

/// The benchmark measures two hover-input lengths.
/// 25 saturates `MAX_LOOKUP_CHARS` and sets the fan-out ceiling.
/// 8 represents a short OCR line.
const SHORT_INPUT: usize = 8;

/// These SQL strings match `src/lookup/sqlite.rs`.
/// The query plans and column-read splits below are valid only when these strings match.
const TERM_SQL: &str = "SELECT surface, written, reading, pos, freq, entry_id, dict_id \
     FROM term WHERE surface = ?1";
const ENTRY_SQL: &str = "SELECT entry_id, dict_id, glossary FROM entry WHERE entry_id = ?1";

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/hover-parse-bench/results")
}

// ---- inputs ----

/// `Record` contains the fields this example needs from one `hover-payloads.jsonl` line.
/// It does not declare the 178 MB `payloads` array.
/// `serde` skips that array and allocates no memory.
#[derive(Deserialize)]
struct Record {
    term: String,
    worst: bool,
}

fn is_jp(c: char) -> bool {
    matches!(c,
        '\u{3041}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{3005}'
        | '\u{3006}')
}

/// `load_fixture` reads the fixture once.
/// It returns the headword sample and real Japanese prose runs from raw glossary
/// text on the same lines.
/// The hover inputs add this prose after the headword.
/// The prefix scan therefore reads realistic characters instead of synthetic filler.
fn load_fixture() -> Result<(Vec<Record>, Vec<String>)> {
    let path = results_dir().join("hover-payloads.jsonl");
    let file = std::fs::File::open(&path).with_context(|| {
        format!("reading {} - run tools/hover-parse-bench/payload.py first", path.display())
    })?;
    let mut records = Vec::new();
    let mut prose: Vec<String> = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str::<Record>(&line).context("parsing a payload record")?,
        );
        let mut per_line = 0;
        let mut run: Vec<char> = Vec::new();
        for c in line.chars() {
            if is_jp(c) {
                run.push(c);
                if run.len() == PROSE_CHARS {
                    if prose.len() < PROSE_POOL && per_line < 8 {
                        prose.push(run.iter().collect());
                        per_line += 1;
                    }
                    run.clear();
                }
            } else {
                run.clear();
            }
        }
    }
    anyhow::ensure!(!records.is_empty(), "empty fixture");
    anyhow::ensure!(!prose.is_empty(), "no Japanese prose harvested from the fixture");
    Ok((records, prose))
}

/// `hover_inputs` builds each input from a headword and real Japanese text.
/// It cuts each input to `chars`.
/// `worker.rs` gives `run` the rest of the OCR line from the cursor (`src/worker.rs:466`).
/// This tail makes the prefix scan issue long probes that always miss.
fn hover_inputs(terms: &[String], prose: &[String], chars: usize) -> Vec<String> {
    terms
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let tail = &prose[i % prose.len()];
            let s: String = t.chars().chain(tail.chars()).take(chars).collect();
            debug_assert_eq!(clean_input(&s), s);
            s
        })
        .collect()
}

fn engine() -> Result<(LookupEngine, Deconjugator)> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/deconjugator.json");
    let rules = load_rules(&p)?;
    Ok((LookupEngine::new(Deconjugator::new(rules.clone())), Deconjugator::new(rules)))
}

// ---- statistics ----

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

struct Row {
    label: String,
    n: usize,
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

fn summarise(label: &str, mut v: Vec<f64>) -> Row {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let total: f64 = v.iter().sum();
    Row {
        label: label.to_string(),
        n,
        mean: if n == 0 { 0.0 } else { total / n as f64 },
        p50: percentile(&v, 0.50),
        p95: percentile(&v, 0.95),
        p99: percentile(&v, 0.99),
        max: v.last().copied().unwrap_or(0.0),
    }
}

fn print_table(title: &str, unit: &str, rows: &[Row]) {
    println!("\n{title}");
    let width = rows.iter().map(|r| r.label.chars().count()).max().unwrap_or(0).max(6);
    println!(
        "{:<width$}  {:>5}  {:>11}  {:>11}  {:>11}  {:>11}  {:>11}",
        "sample",
        "n",
        format!("mean {unit}"),
        format!("p50 {unit}"),
        format!("p95 {unit}"),
        format!("p99 {unit}"),
        format!("max {unit}")
    );
    println!("{}", "-".repeat(width + 2 + 5 + 5 * 13));
    for r in rows {
        println!(
            "{:<width$}  {:>5}  {:>11.2}  {:>11.2}  {:>11.2}  {:>11.2}  {:>11.2}",
            r.label, r.n, r.mean, r.p50, r.p95, r.p99, r.max
        );
    }
}

/// `timer_overhead_us` measures two `Instant::now()` calls around no work.
/// Each per-call number carries about this bias.
/// The instrumented fan-out carries it once per point query, so this bias belongs in the record.
fn timer_overhead_us() -> f64 {
    let mut v = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let t = Instant::now();
        black_box(0u8);
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    median(&mut v)
}

// ---- a second handle, for the column-read splits ----

/// `Raw` uses the same flags and `mmap_size` as `SqliteDictionary::open`.
/// It opens the database read-only and does not use `SQLITE_OPEN_CREATE` or run a migration.
struct Raw {
    conn: Connection,
}

impl Raw {
    fn open(db: &Path) -> Result<Raw> {
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {} read-only", db.display()))?;
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        Ok(Raw { conn })
    }

    /// SQLite alone performs an index seek and one `sqlite3_step` per row.
    /// It decodes no columns. `terms_for` cannot cost less than this floor.
    fn term_step_only(&self, surface: &str) -> Result<usize> {
        let mut stmt = self.conn.prepare_cached(TERM_SQL)?;
        let mut rows = stmt.query([surface])?;
        let mut n = 0usize;
        while rows.next()?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    /// This method reads every column without ownership.
    /// `ValueRef` borrows from the SQLite page.
    /// This is the cost of a zero-copy or arena-backed row read.
    fn term_borrowed(&self, surface: &str) -> Result<u64> {
        let mut stmt = self.conn.prepare_cached(TERM_SQL)?;
        let mut rows = stmt.query([surface])?;
        let mut acc = 0u64;
        while let Some(r) = rows.next()? {
            acc = acc.wrapping_add(r.get_ref(0)?.as_str()?.len() as u64);
            acc = acc.wrapping_add(r.get_ref(1)?.as_str_or_null()?.map_or(0, str::len) as u64);
            acc = acc.wrapping_add(r.get_ref(2)?.as_str_or_null()?.map_or(0, str::len) as u64);
            acc = acc.wrapping_add(r.get_ref(3)?.as_str()?.len() as u64);
            acc = acc.wrapping_add(r.get_ref(4)?.as_i64_or_null()?.unwrap_or(0) as u64);
            acc = acc.wrapping_add(r.get_ref(5)?.as_i64()? as u64);
            acc = acc.wrapping_add(r.get_ref(6)?.as_i64()? as u64);
        }
        Ok(acc)
    }

    /// This measures `entries` without the parse.
    /// It steps through rows and borrows `glossary` TEXT without a copy from SQLite.
    fn glossary_borrowed(&self, ids: &[i64]) -> Result<u64> {
        let mut stmt = self.conn.prepare_cached(ENTRY_SQL)?;
        let mut acc = 0u64;
        for id in ids {
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                acc = acc.wrapping_add(r.get_ref(0)?.as_i64()? as u64);
                acc = acc.wrapping_add(r.get_ref(1)?.as_i64()? as u64);
                acc = acc.wrapping_add(r.get_ref(2)?.as_str()?.len() as u64);
            }
        }
        Ok(acc)
    }

    /// This matches the work that `SqliteDictionary::entries` does before it parses.
    /// It makes one owned heap copy of the JSON per entry.
    /// `entries` used this copy before it borrowed the column into the parser.
    fn glossary_owned(&self, ids: &[i64], out: &mut Vec<String>) -> Result<()> {
        out.clear();
        let mut stmt = self.conn.prepare_cached(ENTRY_SQL)?;
        for id in ids {
            let mut rows = stmt.query([id])?;
            if let Some(r) = rows.next()? {
                let _: i64 = r.get(0)?;
                let _: i64 = r.get(1)?;
                out.push(r.get::<_, String>(2)?);
            }
        }
        Ok(())
    }

    fn explain(&self, sql: &str, param_is_text: bool) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?
            ))
        };
        let out: rusqlite::Result<Vec<String>> = if param_is_text {
            stmt.query_map(["こう"], map)?.collect()
        } else {
            stmt.query_map([1i64], map)?.collect()
        };
        Ok(out?)
    }
}

// ---- counter and time wrapper ----

#[derive(Clone, Copy, Default)]
struct Tally {
    calls: u32,
    hits: u32,
    misses: u32,
    rows: u32,
    hit_ns: u64,
    miss_ns: u64,
    entry_ns: u64,
    entry_ids: u32,
}

/// `Probe` forwards every `Dictionary` call to the real `SqliteDictionary` and times it.
/// `LookupEngine::run` is generic over `D: Dictionary`.
/// This measures the real fan-out instead of a replica.
struct Probe<'a> {
    inner: &'a SqliteDictionary,
    t: Cell<Tally>,
}

impl<'a> Probe<'a> {
    fn new(inner: &'a SqliteDictionary) -> Self {
        Probe { inner, t: Cell::new(Tally::default()) }
    }
    fn take(&self) -> Tally {
        let v = self.t.get();
        self.t.set(Tally::default());
        v
    }
}

impl Dictionary for Probe<'_> {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        let started = Instant::now();
        let out = self.inner.terms_for(surface)?;
        let ns = started.elapsed().as_nanos() as u64;
        let mut t = self.t.get();
        t.calls += 1;
        if out.is_empty() {
            t.misses += 1;
            t.miss_ns += ns;
        } else {
            t.hits += 1;
            t.rows += out.len() as u32;
            t.hit_ns += ns;
        }
        self.t.set(t);
        Ok(out)
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        let started = Instant::now();
        let out = self.inner.entries(ids)?;
        let mut t = self.t.get();
        t.entry_ns += started.elapsed().as_nanos() as u64;
        t.entry_ids += ids.len() as u32;
        self.t.set(t);
        Ok(out)
    }

    fn dicts(&self) -> Result<Vec<DictInfo>> {
        self.inner.dicts()
    }

    /// This method is untimed.
    /// The benchmark measures the term hot path.
    /// A pitch read occurs once per card, not once per surface probe.
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        self.inner.pitch_for(term, reading)
    }
}

/// `Collect` is an untimed twin of `Probe`.
/// It records surfaces that the engine probes and whether each probe hits.
/// The probe benchmark therefore uses the real miss population, not an invented one.
struct Collect<'a> {
    inner: &'a SqliteDictionary,
    hit: RefCell<HashSet<String>>,
    miss: RefCell<HashSet<String>>,
}

impl Dictionary for Collect<'_> {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        let out = self.inner.terms_for(surface)?;
        if out.is_empty() {
            self.miss.borrow_mut().insert(surface.to_string());
        } else {
            self.hit.borrow_mut().insert(surface.to_string());
        }
        Ok(out)
    }
    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        self.inner.entries(ids)
    }
    fn dicts(&self) -> Result<Vec<DictInfo>> {
        self.inner.dicts()
    }
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        self.inner.pitch_for(term, reading)
    }
}

// ---- shared setup ----

struct Bench {
    db: PathBuf,
    label: String,
    dict: SqliteDictionary,
    raw: Raw,
    dict_count: usize,
    /// Frequency-sample headwords that match a row in this database.
    freq: Vec<String>,
    /// This field keeps the deliberately over-represented worst-case headwords separate.
    worst: Vec<String>,
    prose: Vec<String>,
}

fn setup(args: &[String]) -> Result<Bench> {
    let home = std::env::var("HOME").unwrap_or_default();
    let default_db = format!("{home}/.local/share/chibipop/chibipop.sqlite");
    let db = PathBuf::from(args.first().cloned().unwrap_or(default_db));
    let label = args.get(1).cloned().unwrap_or_else(|| "db".to_string());

    let dict = SqliteDictionary::open(&db)?;
    let raw = Raw::open(&db)?;
    let names = dict.dicts()?;
    println!("{label}: {} ({} dictionaries)", db.display(), names.len());
    for d in &names {
        println!("  {:>3}  {}", d.dict_id, d.name);
    }

    let (records, prose) = load_fixture()?;
    let (mut freq, mut worst) = (Vec::new(), Vec::new());
    for r in &records {
        // This pass is untimed and also provides the first cache touch.
        if dict.terms_for(&r.term)?.is_empty() {
            continue;
        }
        if r.worst {
            worst.push(r.term.clone());
        } else {
            freq.push(r.term.clone());
        }
    }
    anyhow::ensure!(!freq.is_empty(), "no frequency-sample headword matched {}", db.display());
    println!(
        "  sample: {} frequency headwords, {} worst-case, {} prose runs, \
         {} of {} fixture terms hit",
        freq.len(),
        worst.len(),
        prose.len(),
        freq.len() + worst.len(),
        records.len(),
    );
    Ok(Bench { db, label, dict, raw, dict_count: names.len(), freq, worst, prose })
}

/// `warm` reads every item once and records no time.
/// A timed sweep therefore pays no first-touch cost.
fn warm(b: &Bench, items: &[String]) -> Result<()> {
    for s in items {
        let rows = b.dict.terms_for(s)?;
        let ids: Vec<i64> = rows.iter().take(MAX_RESULTS).map(|r| r.entry_id).collect();
        black_box(b.dict.entries(&ids)?);
    }
    Ok(())
}

/// `sweep` runs `SWEEPS` passes over `items` and keeps each item's median.
/// `f` returns microseconds for one item.
fn sweep<T>(items: &[T], mut f: impl FnMut(&T) -> Result<f64>) -> Result<Vec<f64>> {
    let mut samples: Vec<Vec<f64>> = vec![Vec::with_capacity(SWEEPS); items.len()];
    for _ in 0..SWEEPS {
        for (i, it) in items.iter().enumerate() {
            samples[i].push(f(it)?);
        }
    }
    Ok(samples.iter_mut().map(|s| median(s)).collect())
}

fn time_us(mut f: impl FnMut() -> Result<()>) -> Result<f64> {
    let started = Instant::now();
    f()?;
    Ok(started.elapsed().as_secs_f64() * 1e6)
}

// ---- mode: probe ----

/// `probed_surfaces` returns every surface that the engine probes in one pass
/// over realistic hover inputs.
/// It separates surfaces by hit result.
/// This function is untimed.
fn probed_surfaces(b: &Bench, inputs: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let (eng, _) = engine()?;
    let c = Collect {
        inner: &b.dict,
        hit: RefCell::new(HashSet::new()),
        miss: RefCell::new(HashSet::new()),
    };
    for input in inputs {
        black_box(eng.run(&c, input)?);
    }
    let mut hit: Vec<String> = c.hit.into_inner().into_iter().collect();
    let mut miss: Vec<String> = c.miss.into_inner().into_iter().collect();
    hit.sort();
    miss.sort();
    Ok((hit, miss))
}

fn stride(v: Vec<String>, cap: usize) -> Vec<String> {
    if v.len() <= cap {
        return v;
    }
    let step = v.len() as f64 / cap as f64;
    (0..cap).map(|i| v[(i as f64 * step) as usize].clone()).collect()
}

fn mode_probe(b: &Bench) -> Result<()> {
    let inputs = hover_inputs(&b.freq, &b.prose, MAX_LOOKUP_CHARS);
    let (hit_surfaces, engine_misses) = probed_surfaces(b, &inputs)?;

    // Build a second independent miss set from sampled headwords with one more kana.
    // `ゐ` is a historical kana that no modern headword ends with.
    // Confirm each string is absent before the benchmark keeps it.
    // These plausible non-words avoid absurd inputs.
    let mut glued = Vec::new();
    for t in &b.freq {
        let s = format!("{t}ゐ");
        if b.dict.terms_for(&s)?.is_empty() {
            glued.push(s);
        }
    }

    let engine_misses = stride(engine_misses, MAX_MISS_PROBES);
    let hit_surfaces = stride(hit_surfaces, MAX_MISS_PROBES);

    warm(b, &b.freq)?;
    for s in engine_misses.iter().chain(glued.iter()).chain(hit_surfaces.iter()) {
        black_box(b.dict.terms_for(s)?);
    }

    let probe = |s: &String| time_us(|| Ok(black_box(b.dict.terms_for(s)?)).map(|_| ()));

    let freq_hit = sweep(&b.freq, probe)?;
    let any_hit = sweep(&hit_surfaces, probe)?;
    let miss_engine = sweep(&engine_misses, probe)?;
    let miss_glued = sweep(&glued, probe)?;
    let worst_hit = if b.worst.is_empty() { Vec::new() } else { sweep(&b.worst, probe)? };

    let mut rows = vec![
        summarise("hit: frequency headwords", freq_hit),
        summarise("hit: every surface the engine probed and found", any_hit),
        summarise("MISS: every surface the engine probed and missed", miss_engine),
        summarise("MISS: headword + `ゐ` (confirmed absent)", miss_glued),
    ];
    if !worst_hit.is_empty() {
        rows.push(summarise("hit: worst-case headwords", worst_hit));
    }
    print_table(&format!("{}: terms_for, per call", b.label), "µs", &rows);

    // This shows where a miss's 4 µs goes.
    // `term_step_only` runs `prepare_cached`, bind, and one `sqlite3_step` that returns DONE.
    // It decodes no columns.
    // `terms_for` adds rusqlite row conversion, which returns only an empty `Vec` for a miss.
    // Equal values mean SQLite's b-tree descent accounts for the full cost.
    // Rust-side work cannot remove that descent.
    let step = |s: &String| {
        time_us(|| {
            black_box(b.raw.term_step_only(s)?);
            Ok(())
        })
    };
    let miss_step = sweep(&engine_misses, step)?;
    let miss_full = sweep(&engine_misses, probe)?;
    let miss_rust: Vec<f64> =
        miss_full.iter().zip(&miss_step).map(|(f, s)| f - s).collect();
    print_table(
        &format!("{}: what a negative lookup costs, decomposed", b.label),
        "µs",
        &[
            summarise("sqlite alone: prepare_cached + bind + step -> DONE", miss_step),
            summarise("SqliteDictionary::terms_for (same key)", miss_full),
            summarise("  rusqlite/Rust overhead on top (empty Vec)", miss_rust),
        ],
    );

    // Use the same three-way split on the frequency sample.
    // This breaks down the 63.3 µs hover that `hover-parse-cost.md` reports.
    let hit_step = sweep(&b.freq, step)?;
    let hit_borrowed = sweep(&b.freq, |s| {
        time_us(|| {
            black_box(b.raw.term_borrowed(s)?);
            Ok(())
        })
    })?;
    let hit_mapped = sweep(&b.freq, probe)?;
    let hit_alloc: Vec<f64> =
        hit_mapped.iter().zip(&hit_borrowed).map(|(m, bo)| m - bo).collect();
    print_table(
        &format!("{}: terms_for on a frequency headword, decomposed", b.label),
        "µs",
        &[
            summarise("sqlite alone: index seek + step, no column decoded", hit_step),
            summarise("+ every column decoded, borrowed (zero-copy)", hit_borrowed),
            summarise("+ mapped to Vec<TermRow> (what terms_for returns)", hit_mapped),
            summarise("  row-mapping allocation alone (mapped - borrowed)", hit_alloc),
        ],
    );

    let mut rowcounts = Vec::new();
    for t in &b.freq {
        rowcounts.push(b.dict.terms_for(t)?.len() as f64);
    }
    let mut worst_counts = Vec::new();
    for t in &b.worst {
        worst_counts.push(b.dict.terms_for(t)?.len() as f64);
    }
    let mut counts = vec![summarise("rows per hit, frequency headwords", rowcounts)];
    if !worst_counts.is_empty() {
        counts.push(summarise("rows per hit, worst-case headwords", worst_counts));
    }
    print_table(&format!("{}: terms_for, rows returned", b.label), "rows", &counts);

    // ---- prepared-statement reuse ----
    // The sweeps above warm the pages.
    // `SqliteDictionary::open` already forces the schema parse with its `meta` query.
    // The cold/warm delta therefore measures `sqlite3_prepare_v2` for the term
    // statement and nothing else.
    let n = b.freq.len().min(300);
    let (mut cold, mut warm_after) = (Vec::new(), Vec::new());
    for t in b.freq.iter().take(n) {
        let fresh = SqliteDictionary::open(&b.db)?;
        cold.push(time_us(|| Ok(black_box(fresh.terms_for(t)?)).map(|_| ()))?);
        let mut later: Vec<f64> = Vec::new();
        for _ in 0..SWEEPS {
            later.push(time_us(|| Ok(black_box(fresh.terms_for(t)?)).map(|_| ()))?);
        }
        warm_after.push(median(&mut later));
    }
    let mut compile = Vec::new();
    let mut cached = Vec::new();
    for _ in 0..(SWEEPS * 200) {
        compile.push(time_us(|| {
            black_box(b.raw.conn.prepare(TERM_SQL)?);
            Ok(())
        })?);
        cached.push(time_us(|| {
            black_box(b.raw.conn.prepare_cached(TERM_SQL)?);
            Ok(())
        })?);
    }
    print_table(
        &format!("{}: prepared-statement reuse", b.label),
        "µs",
        &[
            summarise("first terms_for on a fresh connection (cold cache)", cold),
            summarise("same terms_for, steady state (warm cache)", warm_after),
            summarise("conn.prepare(term sql) - compile every time", compile),
            summarise("conn.prepare_cached(term sql) - cache hit", cached),
        ],
    );
    println!(
        "\n{}: timer overhead per Instant::now pair = {:.3} µs",
        b.label,
        timer_overhead_us()
    );
    Ok(())
}

// ---- mode: fanout ----

struct FanOut {
    calls: Vec<f64>,
    hits: Vec<f64>,
    misses: Vec<f64>,
    rows: Vec<f64>,
    ids: Vec<f64>,
    plain_total: Vec<f64>,
    instr_total: Vec<f64>,
    hit_us: Vec<f64>,
    miss_us: Vec<f64>,
    entry_us: Vec<f64>,
    deconj_us: Vec<f64>,
    residual: Vec<f64>,
}

/// This is the deconjugation half of `run`'s prefix loop, verbatim from
/// `src/lookup/engine.rs:109-115` except for the dictionary probe.
/// The benchmark measures it out of band because `run` has no internal seam.
fn deconj_only(d: &Deconjugator, input: &str) -> f64 {
    let cleaned = clean_input(input);
    let chars: Vec<char> = cleaned.chars().collect();
    let started = Instant::now();
    for prefix_len in (1..=chars.len()).rev() {
        let prefix: String = chars[..prefix_len].iter().collect();
        let forms: Vec<Form> = d.deconjugate(&prefix).into_iter().collect();
        black_box(&forms);
    }
    started.elapsed().as_secs_f64() * 1e6
}

fn measure_fanout(b: &Bench, inputs: &[String]) -> Result<FanOut> {
    let (eng, dec) = engine()?;
    let probe = Probe::new(&b.dict);
    for input in inputs {
        black_box(eng.run(&probe, input)?);
        probe.take();
    }

    let n = inputs.len();
    let mut s: Vec<Vec<[f64; 8]>> = vec![Vec::with_capacity(SWEEPS); n];
    for _ in 0..SWEEPS {
        for (i, input) in inputs.iter().enumerate() {
            let started = Instant::now();
            let hits = eng.run(&b.dict, input)?;
            let plain = started.elapsed().as_secs_f64() * 1e6;
            black_box(&hits);

            let started = Instant::now();
            let hits = eng.run(&probe, input)?;
            let instr = started.elapsed().as_secs_f64() * 1e6;
            black_box(&hits);
            let t = probe.take();

            let deconj = deconj_only(&dec, input);
            let hit_us = t.hit_ns as f64 / 1e3;
            let miss_us = t.miss_ns as f64 / 1e3;
            let entry_us = t.entry_ns as f64 / 1e3;
            s[i].push([
                plain,
                instr,
                hit_us,
                miss_us,
                entry_us,
                deconj,
                plain - hit_us - miss_us - entry_us - deconj,
                t.calls as f64,
            ]);
        }
    }

    // Counts are deterministic per input. Read them from one clean pass.
    let (mut calls, mut hits, mut misses, mut rows, mut ids) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for input in inputs {
        black_box(eng.run(&probe, input)?);
        let t = probe.take();
        calls.push(t.calls as f64);
        hits.push(t.hits as f64);
        misses.push(t.misses as f64);
        rows.push(t.rows as f64);
        ids.push(t.entry_ids as f64);
    }

    let col = |k: usize| -> Vec<f64> {
        (0..n)
            .map(|i| {
                let mut v: Vec<f64> = s[i].iter().map(|r| r[k]).collect();
                median(&mut v)
            })
            .collect()
    };
    Ok(FanOut {
        calls,
        hits,
        misses,
        rows,
        ids,
        plain_total: col(0),
        instr_total: col(1),
        hit_us: col(2),
        miss_us: col(3),
        entry_us: col(4),
        deconj_us: col(5),
        residual: col(6),
    })
}

fn report_fanout(b: &Bench, title: &str, inputs: &[String], f: FanOut) {
    print_table(
        &format!("{}: {title} - point queries LookupEngine::run issues per hover", b.label),
        "n",
        &[
            summarise("terms_for calls", f.calls.clone()),
            summarise("  of which HIT", f.hits.clone()),
            summarise("  of which MISS", f.misses.clone()),
            summarise("term rows returned in total", f.rows.clone()),
            summarise("entry ids passed to entries()", f.ids),
        ],
    );
    let fan: Vec<f64> =
        f.hit_us.iter().zip(&f.miss_us).map(|(a, b)| a + b).collect();
    print_table(
        &format!("{}: {title} - one real hover, split", b.label),
        "µs",
        &[
            summarise("LookupEngine::run, total (uninstrumented)", f.plain_total.clone()),
            summarise("  deconjugation (prefix loop, out-of-band replica)", f.deconj_us.clone()),
            summarise("  terms_for on hits", f.hit_us.clone()),
            summarise("  terms_for on misses", f.miss_us.clone()),
            summarise("  fan-out subtotal (hits + misses)", fan),
            summarise("  entries() incl. the GlossDoc parse", f.entry_us.clone()),
            summarise("  residual: group + rank + sort + build Hits", f.residual.clone()),
            summarise("run, instrumented (shows probe overhead)", f.instr_total),
        ],
    );
    // The max column takes maxima from different inputs.
    // It does not split one hover.
    // Name the single slowest hover and split that hover for an honest worst-case row.
    if let Some((i, worst)) = f
        .plain_total
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
    {
        println!(
            "  slowest single hover: 「{}」 {:.1} µs = {:.0} point queries \
             ({:.0} hit / {:.0} miss), {:.0} term rows\n    \
             deconj {:.1} | terms_for hits {:.1} | terms_for misses {:.1} | \
             entries {:.1} | group+rank+sort {:.1}",
            inputs[i],
            worst,
            f.calls[i],
            f.hits[i],
            f.misses[i],
            f.rows[i],
            f.deconj_us[i],
            f.hit_us[i],
            f.miss_us[i],
            f.entry_us[i],
            f.residual[i],
        );
    }
}

fn mode_fanout(b: &Bench) -> Result<()> {
    warm(b, &b.freq)?;
    let long = hover_inputs(&b.freq, &b.prose, MAX_LOOKUP_CHARS);
    let short = hover_inputs(&b.freq, &b.prose, SHORT_INPUT);
    let bare = b.freq.clone();

    report_fanout(
        b,
        &format!("{MAX_LOOKUP_CHARS}-char line"),
        &long,
        measure_fanout(b, &long)?,
    );
    report_fanout(b, &format!("{SHORT_INPUT}-char line"), &short, measure_fanout(b, &short)?);
    report_fanout(b, "bare headword, no tail", &bare, measure_fanout(b, &bare)?);
    println!(
        "\n{}: timer overhead per Instant::now pair = {:.3} µs \
         (the instrumented row pays two per point query)",
        b.label,
        timer_overhead_us()
    );
    Ok(())
}

// ---- mode: entries ----

fn mode_entries(b: &Bench) -> Result<()> {
    warm(b, &b.freq)?;
    // This is what a hover passes to `entries`: the top `MAX_RESULTS` ids.
    let mut idsets: Vec<Vec<i64>> = Vec::new();
    for t in &b.freq {
        let rows = b.dict.terms_for(t)?;
        idsets.push(rows.iter().take(MAX_RESULTS).map(|r| r.entry_id).collect());
    }
    let mut bytes = 0u64;
    let mut buf: Vec<String> = Vec::new();
    for ids in &idsets {
        b.raw.glossary_owned(ids, &mut buf)?;
        bytes += buf.iter().map(|s| s.len() as u64).sum::<u64>();
    }

    let scratch = RefCell::new(Vec::<String>::new());
    let step = sweep(&idsets, |ids| {
        time_us(|| {
            black_box(b.raw.glossary_borrowed(ids)?);
            Ok(())
        })
    })?;
    let owned = sweep(&idsets, |ids| {
        time_us(|| {
            b.raw.glossary_owned(ids, &mut scratch.borrow_mut())?;
            black_box(&*scratch.borrow());
            Ok(())
        })
    })?;
    // Parse only.
    // The strings are already in hand from outside the timed region.
    // This measures only serde.
    let prefetched: Vec<Vec<String>> = {
        let mut v: Vec<Vec<String>> = Vec::with_capacity(idsets.len());
        for ids in &idsets {
            let mut one = Vec::new();
            b.raw.glossary_owned(ids, &mut one)?;
            v.push(one);
        }
        v
    };
    for v in &prefetched {
        for s in v {
            black_box(GlossDoc::parse(s));
        }
    }
    let parsed = sweep(&prefetched, |v| {
        time_us(|| {
            for s in v {
                black_box(GlossDoc::parse(s));
            }
            Ok(())
        })
    })?;
    let whole = sweep(&idsets, |ids| {
        time_us(|| {
            black_box(b.dict.entries(ids)?);
            Ok(())
        })
    })?;
    // The `hover-parse-cost.md` headline "hover today" reproduces a known number.
    // `examples/hover_parse_bench.rs:433-439` measures one `terms_for` call and
    // `entries` over the first `MAX_RESULTS` ids as one timed region.
    // Keep this anchor so the whole breakdown uses that number.
    let doc = sweep(&b.freq, |t| {
        time_us(|| {
            let rows = b.dict.terms_for(t)?;
            let ids: Vec<i64> =
                rows.iter().take(MAX_RESULTS).map(|r| r.entry_id).collect();
            black_box(b.dict.entries(&ids)?);
            Ok(())
        })
    })?;

    let copy: Vec<f64> = owned.iter().zip(&step).map(|(o, s)| o - s).collect();
    let total: f64 = whole.iter().sum();
    print_table(
        &format!(
            "{}: entries() for the top {MAX_RESULTS} ids, split \
             ({:.1} MB of glossary json over {} headwords)",
            b.label,
            bytes as f64 / 1e6,
            idsets.len()
        ),
        "µs",
        &[
            summarise("SQL fetch, borrowed (step + column, no copy)", step.clone()),
            summarise("SQL fetch, owned String (what entries() does)", owned),
            summarise("  the String copy alone (owned - borrowed)", copy),
            summarise("GlossDoc::parse", parsed.clone()),
            summarise("SqliteDictionary::entries, whole", whole),
            summarise("terms_for + entries(top 10) = hover-parse-cost.md's 63.3 µs", doc),
        ],
    );
    let parse_total: f64 = parsed.iter().sum();
    let step_total: f64 = step.iter().sum();
    println!(
        "\n{}: aggregate share of entries() - sqlite step+column {:.1}%, \
         serde parse {:.1}%; serde throughput {:.0} MB/s",
        b.label,
        100.0 * step_total / total,
        100.0 * parse_total / total,
        bytes as f64 / parse_total,
    );
    Ok(())
}

/// This matches `engine.rs:30-38` verbatim.
/// `DEFAULT_FREQ` is private, so this function uses its value, `999_999.0`, from
/// `engine.rs:11`.
fn replica_score(match_len: usize, freq: Option<i64>, kana_bonus: bool) -> f64 {
    const DEFAULT_FREQ: f64 = 999_999.0;
    let f = freq.map(|v| v as f64).unwrap_or(DEFAULT_FREQ).max(1.0);
    let mut s = match_len as f64;
    s += 10.0 * (1.0 - f.ln() / DEFAULT_FREQ.ln());
    if kana_bonus {
        s += 3.0;
    }
    s
}

fn replica_kana(r: &TermRow) -> bool {
    r.surface
        .chars()
        .all(|c| matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}'))
}

/// This matches `engine.rs:135-154`.
/// It keeps one slot per `(written, reading, dict_id)` and clones two `String`
/// values per row to build the key.
fn regroup(rows: &[TermRow]) -> Vec<TermRow> {
    let mut best: HashMap<(Option<String>, Option<String>, i64), TermRow> =
        HashMap::with_capacity(rows.len());
    for row in rows {
        let key = (row.written.clone(), row.reading.clone(), row.dict_id);
        match best.get(&key) {
            Some(existing) if existing.entry_id <= row.entry_id => {}
            _ => {
                best.insert(key, row.clone());
            }
        }
    }
    best.into_values().collect()
}

/// This matches `engine.rs:156-174`.
/// At one surface and one match length, these keys still distinguish rows.
fn rank_cmp(a: &TermRow, b: &TermRow) -> std::cmp::Ordering {
    let sa = replica_score(1, a.freq, replica_kana(a));
    let sb = replica_score(1, b.freq, replica_kana(b));
    sb.partial_cmp(&sa)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.dict_id.cmp(&b.dict_id))
        .then(a.entry_id.cmp(&b.entry_id))
}

// ---- mode: worst ----

fn mode_worst(b: &Bench) -> Result<()> {
    if b.worst.is_empty() {
        println!("\n{}: no worst-case headword matched; skipping", b.label);
        return Ok(());
    }
    warm(b, &b.worst)?;
    let items = b.worst.clone();

    let step = sweep(&items, |t| {
        time_us(|| {
            black_box(b.raw.term_step_only(t)?);
            Ok(())
        })
    })?;
    let borrowed = sweep(&items, |t| {
        time_us(|| {
            black_box(b.raw.term_borrowed(t)?);
            Ok(())
        })
    })?;
    let mapped = sweep(&items, |t| {
        time_us(|| {
            black_box(b.dict.terms_for(t)?);
            Ok(())
        })
    })?;
    let doc = sweep(&items, |t| {
        time_us(|| {
            let rows = b.dict.terms_for(t)?;
            let ids: Vec<i64> = rows.iter().take(MAX_RESULTS).map(|r| r.entry_id).collect();
            black_box(b.dict.entries(&ids)?);
            Ok(())
        })
    })?;
    let alloc: Vec<f64> = mapped.iter().zip(&borrowed).map(|(m, bo)| m - bo).collect();

    print_table(
        &format!("{}: worst-case headwords, terms_for split", b.label),
        "µs",
        &[
            summarise("sqlite alone: index seek + step, no column decoded", step),
            summarise("+ every column decoded, borrowed (zero-copy)", borrowed),
            summarise("+ mapped to Vec<TermRow> (what terms_for returns)", mapped.clone()),
            summarise("  row-mapping allocation alone (mapped - borrowed)", alloc.clone()),
            summarise("terms_for + entries(top 10) - the doc's number", doc),
        ],
    );

    // The instrumented run reports the group step, sort step, and `Hit` creation
    // as one residual.
    // `Candidate`, `score`, and `is_better` are private to `engine.rs`.
    // This `REPLICA` therefore apportions that residual with the same logic from
    // `engine.rs:135-175`.
    // It uses the same `GroupKey`, two `String` clones per row, and `score` from
    // `engine.rs:30-38`.
    // A bare headword has one surface and one match length.
    // Therefore, `match_len`, `whole`, and `steps` stay constant, and the real
    // comparator reduces to the three keys here.
    //
    // The benchmark times the group step as A and the group-and-sort step as A+B.
    // This avoids order restoration between sweeps.
    // An already-sorted vector gives the wrong measure.
    let fetched: Vec<Vec<TermRow>> = items
        .iter()
        .map(|t| b.dict.terms_for(t))
        .collect::<Result<_>>()?;
    let group_only = sweep(&fetched, |rows| {
        time_us(|| {
            black_box(regroup(rows).len());
            Ok(())
        })
    })?;
    let group_sort = sweep(&fetched, |rows| {
        time_us(|| {
            let mut ranked = regroup(rows);
            ranked.sort_by(rank_cmp);
            ranked.truncate(MAX_RESULTS);
            black_box(&ranked);
            Ok(())
        })
    })?;
    let sort_only: Vec<f64> =
        group_sort.iter().zip(&group_only).map(|(gs, g)| gs - g).collect();
    let groups: Vec<f64> = fetched.iter().map(|r| regroup(r).len() as f64).collect();
    let rowcount: Vec<f64> = fetched.iter().map(|r| r.len() as f64).collect();
    print_table(
        &format!(
            "{}: worst-case headwords, what happens to the rows after terms_for \
             (REPLICA of engine.rs:135-175)",
            b.label
        ),
        "µs",
        &[
            summarise("group into HashMap<GroupKey, _> (2 String clones/row)", group_only),
            summarise("group + sort + truncate(MAX_RESULTS)", group_sort),
            summarise("  the sort alone (difference)", sort_only),
        ],
    );
    print_table(
        &format!("{}: worst-case headwords, rows in vs groups out", b.label),
        "n",
        &[
            summarise("term rows from terms_for", rowcount),
            summarise("distinct (written, reading, dict_id) groups the sort sees", groups),
        ],
    );

    let f = measure_fanout(b, &items)?;
    report_fanout(b, "worst-case headword, bare", &items, f);

    // The single headword named in the document.
    if let Some(k) = items.iter().find(|t| t.as_str() == "こう") {
        let rows = b.dict.terms_for(k)?;
        let one = |f: &dyn Fn() -> Result<()>| -> Result<f64> {
            let mut v = Vec::new();
            for _ in 0..(SWEEPS * 4) {
                let started = Instant::now();
                f()?;
                v.push(started.elapsed().as_secs_f64() * 1e6);
            }
            Ok(median(&mut v))
        };
        println!(
            "\n{}: 「{k}」 alone - {} term rows\n  \
             sqlite step only          {:>9.1} µs\n  \
             + borrowed column decode  {:>9.1} µs\n  \
             + Vec<TermRow> mapping    {:>9.1} µs  (allocation delta {:.1} µs)",
            b.label,
            rows.len(),
            one(&|| {
                black_box(b.raw.term_step_only(k)?);
                Ok(())
            })?,
            one(&|| {
                black_box(b.raw.term_borrowed(k)?);
                Ok(())
            })?,
            one(&|| {
                black_box(b.dict.terms_for(k)?);
                Ok(())
            })?,
            one(&|| {
                black_box(b.dict.terms_for(k)?);
                Ok(())
            })? - one(&|| {
                black_box(b.raw.term_borrowed(k)?);
                Ok(())
            })?,
        );
    }
    Ok(())
}

// ---- mode: plan ----

fn mode_plan(b: &Bench) -> Result<()> {
    println!("\n{}: EXPLAIN QUERY PLAN, verbatim (id|parent|notused|detail)", b.label);
    println!("\n  -- terms_for, src/lookup/sqlite.rs:52-55\n  {TERM_SQL}");
    for l in b.raw.explain(TERM_SQL, true)? {
        println!("  {l}");
    }
    println!("\n  -- entries, src/lookup/sqlite.rs:75-77\n  {ENTRY_SQL}");
    for l in b.raw.explain(ENTRY_SQL, false)? {
        println!("  {l}");
    }
    let names: Vec<String> = b
        .raw
        .conn
        .prepare("SELECT name || ' ON ' || tbl_name FROM sqlite_master WHERE type='index'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    println!("\n  indexes present ({} dictionaries):", b.dict_count);
    for n in &names {
        println!("    {n}");
    }
    Ok(())
}

// ---- mode: bloom ----

/// The filter uses ten bits per key and seven hashes.
/// This is the textbook optimum with ~0.8% false positives.
/// A filter for a library with twelve Dictionaries fits in a few megabytes and stays resident.
const BLOOM_BITS_PER_KEY: usize = 10;
const BLOOM_HASHES: u32 = 7;

struct Bloom {
    bits: Vec<u64>,
    mask: usize,
}

/// `fnv1a` implements FNV-1a.
/// A real implementation can use a faster hash (`hoshidicts` uses xxHash).
/// The probe cost below is therefore a ceiling, not a floor.
fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl Bloom {
    fn new(keys: usize) -> Bloom {
        let want = keys * BLOOM_BITS_PER_KEY;
        let words = (want / 64 + 1).next_power_of_two();
        Bloom { bits: vec![0u64; words], mask: words * 64 - 1 }
    }
    fn bytes(&self) -> usize {
        self.bits.len() * 8
    }
    /// Kirsch-Mitzenmacher uses two hashes to stand in for `BLOOM_HASHES`.
    #[inline]
    fn slots(&self, key: &str) -> impl Iterator<Item = usize> + '_ {
        let h1 = fnv1a(key.as_bytes(), 0);
        let h2 = fnv1a(key.as_bytes(), 0x9e37_79b9_7f4a_7c15);
        let mask = self.mask;
        (0..BLOOM_HASHES as u64).map(move |i| (h1.wrapping_add(h2.wrapping_mul(i)) as usize) & mask)
    }
    fn insert(&mut self, key: &str) {
        for s in self.slots(key).collect::<Vec<_>>() {
            self.bits[s >> 6] |= 1u64 << (s & 63);
        }
    }
    #[inline]
    fn maybe_contains(&self, key: &str) -> bool {
        self.slots(key).all(|s| self.bits[s >> 6] & (1u64 << (s & 63)) != 0)
    }
}

/// This function builds and probes a bloom filter over `term.surface`.
/// It measures the savings.
/// It uses a model only for the final arithmetic.
fn mode_bloom(b: &Bench) -> Result<()> {
    let started = Instant::now();
    let mut surfaces = 0usize;
    let distinct: Vec<String> = {
        let mut stmt = b.raw.conn.prepare("SELECT DISTINCT surface FROM term")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut v = Vec::new();
        for s in rows {
            v.push(s?);
            surfaces += 1;
        }
        v
    };
    let scan = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut bloom = Bloom::new(distinct.len());
    for s in &distinct {
        bloom.insert(s);
    }
    let build = started.elapsed().as_secs_f64();
    println!(
        "\n{}: bloom over term.surface - {} distinct surfaces, {:.1} MB filter, \
         {BLOOM_BITS_PER_KEY} bits/key, k={BLOOM_HASHES} \
         (scan {:.1}s, build {:.1}s)",
        b.label,
        surfaces,
        bloom.bytes() as f64 / 1e6,
        scan,
        build,
    );

    let inputs = hover_inputs(&b.freq, &b.prose, MAX_LOOKUP_CHARS);
    let (_, engine_misses) = probed_surfaces(b, &inputs)?;
    let n_miss = engine_misses.len();
    let fp = engine_misses.iter().filter(|s| bloom.maybe_contains(s)).count();
    let present = distinct.iter().filter(|s| !bloom.maybe_contains(s)).count();
    anyhow::ensure!(present == 0, "bloom filter reported a false negative - broken");

    let misses = stride(engine_misses, MAX_MISS_PROBES);
    for s in &misses {
        black_box(bloom.maybe_contains(s));
    }
    // One probe takes less than one microsecond.
    // Repeat it inside the timed region and divide the result because one probe
    // takes about as long as the timer overhead.
    const REPS: usize = 64;
    let probe_us = sweep(&misses, |s| {
        Ok(time_us(|| {
            for _ in 0..REPS {
                black_box(bloom.maybe_contains(black_box(s)));
            }
            Ok(())
        })? / REPS as f64)
    })?;
    let sqlite_us = sweep(&misses, |s| {
        time_us(|| {
            black_box(b.dict.terms_for(s)?);
            Ok(())
        })
    })?;
    print_table(
        &format!("{}: negative lookup, bloom probe vs the SQLite probe it replaces", b.label),
        "µs",
        &[
            summarise("bloom maybe_contains (in memory)", probe_us.clone()),
            summarise("SqliteDictionary::terms_for (same keys)", sqlite_us.clone()),
        ],
    );

    // A per-key sweep keeps one key's seven cache lines hot.
    // Time one full sweep over 3 000 distinct keys and divide by the key count.
    // This cold-line number is the value to report.
    let mut swept = Vec::new();
    for _ in 0..SWEEPS {
        let us = time_us(|| {
            for s in &misses {
                black_box(bloom.maybe_contains(s));
            }
            Ok(())
        })?;
        swept.push(us / misses.len() as f64);
    }
    println!(
        "  bloom swept over {} distinct keys: {:.3} µs per probe (median of {SWEEPS} sweeps)",
        misses.len(),
        median(&mut swept),
    );

    let p50 = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        percentile(&v, 0.50)
    };
    let bloom_p50 = p50(&probe_us);
    let sqlite_p50 = p50(&sqlite_us);
    let fpr = fp as f64 / n_miss as f64;
    println!(
        "  false positives: {fp} of {n_miss} real engine misses = {:.2}% \
         (a false positive still pays the SQLite probe)",
        100.0 * fpr
    );
    println!(
        "  saving per rejected miss: {:.3} - {:.3} = {:.3} µs; \
         net of false positives {:.3} µs",
        sqlite_p50,
        bloom_p50,
        sqlite_p50 - bloom_p50,
        (1.0 - fpr) * (sqlite_p50 - bloom_p50),
    );

    // This estimates the result for one whole hover and one input.
    // It uses the same 25-char lines as fan-out mode.
    // All values are measured except the assumption that a false positive costs
    // the average miss: [MODELLED].
    let f = measure_fanout(b, &inputs)?;
    let mean_miss: f64 = sqlite_us.iter().sum::<f64>() / sqlite_us.len() as f64;
    let with: Vec<f64> = (0..inputs.len())
        .map(|i| {
            f.plain_total[i] - f.miss_us[i]
                + f.misses[i] * fpr * mean_miss
                + f.calls[i] * bloom_p50
        })
        .collect();
    let saved: Vec<f64> =
        (0..inputs.len()).map(|i| f.plain_total[i] - with[i]).collect();
    print_table(
        &format!("{}: one 25-char hover, with and without a surface bloom", b.label),
        "µs",
        &[
            summarise("LookupEngine::run today", f.plain_total.clone()),
            summarise("same hover, misses rejected by the bloom", with),
            summarise("saved", saved),
        ],
    );
    Ok(())
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mode = argv.first().map(String::as_str).unwrap_or("");
    let rest = if argv.is_empty() { &[][..] } else { &argv[1..] };
    let b = match mode {
        "probe" | "fanout" | "entries" | "worst" | "plan" | "bloom" | "all" => setup(rest)?,
        _ => {
            anyhow::bail!(
                "usage: lookup_breakdown \
                 <probe|fanout|entries|worst|plan|bloom|all> [db] [label]"
            )
        }
    };
    match mode {
        "probe" => mode_probe(&b)?,
        "fanout" => mode_fanout(&b)?,
        "entries" => mode_entries(&b)?,
        "worst" => mode_worst(&b)?,
        "plan" => mode_plan(&b)?,
        "bloom" => mode_bloom(&b)?,
        "all" => {
            mode_plan(&b)?;
            mode_probe(&b)?;
            mode_entries(&b)?;
            mode_fanout(&b)?;
            mode_worst(&b)?;
            mode_bloom(&b)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}
