//! This benchmark measures one hover in three ways.
//!
//! It compares parse at build time with parse at hover time.
//! One plan parses structured content at build time and stores a typed tree.
//! That plan assumes a hover performs about 25 point queries and cannot include a parse.
//!
//! Another plan stores raw Yomitan JSON and parses it at hover time.
//! This plan lets developers fix parser defects without a schema change.
//! Parse time decides between the plans, so this benchmark measures that time.
//!
//! Each mode prints a table to standard output:
//!
//! ```sh
//! cargo run -p chibipop --release --example hover_parse_bench -- parse
//! cargo run -p chibipop --release --example hover_parse_bench -- build <out.sqlite> [corpus]
//! cargo run -p chibipop --release --example hover_parse_bench -- hover [db] [label]
//! ```
//!
//! `parse` is the alternative case.
//! It applies `serde_json` to real glossary payloads that
//! `tools/hover-parse-bench/payload.py` extracted.
//! It performs a full recursive walk of each tree.
//! The caller consumes the walk result, so the optimizer keeps the parse.
//! It includes traversal cost similar to the cost of a typed tree build.
//!
//! `build` writes a temporary multi-Dictionary database from the `library`
//! list in `results/summary.json`.
//! It uses the same twelve archives that `payload.py` used for its "realistic library" column.
//! The shared list prevents drift.
//! The mode calls chibipop's `dict::build::build`.
//! Set a temporary path, not the user's
//! `~/.local/share/chibipop/chibipop.sqlite`.
//!
//! `hover` is the baseline.
//! It calls `terms_for` and then `entries` against a database.
//! This measurement includes the `GlossDoc` parse that `entries` performs.
//! The mode opens the database through `SqliteDictionary::open`.
//! The open flags include `SQLITE_OPEN_READ_ONLY` but not
//! `SQLITE_OPEN_CREATE`.
//! The mode runs no migration and does not modify the database.
//!
//! Release mode is required. A debug-profile serde result is not valid for this comparison.

use anyhow::{Context, Result};
use chibipop::dict::build::build;
use chibipop::dict::gloss::{GlossDoc, Tag};
use chibipop::lookup::engine::MAX_RESULTS;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `MIN_SAMPLE` sets the minimum duration for each term sample.
/// Below this duration, `Instant` resolution and cache noise dominate one parse.
const MIN_SAMPLE: Duration = Duration::from_millis(50);

/// `HOVER_SWEEPS` sets the number of full term-list sweeps in `hover` mode.
///
/// Repeated calls to one term measure a cache state that an actual hover does not have.
/// The median for each term keeps the access pattern close to actual hovers.
const HOVER_SWEEPS: usize = 5;

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/hover-parse-bench/results")
}

// ---- inputs ----

/// One line of `hover-payloads.jsonl`.
#[derive(Deserialize)]
struct Record {
    term: String,
    worst: bool,
    rows: usize,
    bytes: usize,
    lib_rows: usize,
    lib_bytes: usize,
    /// The scan reads the library archives first.
    /// Therefore, `payloads[..lib_rows]` is the library subset.
    /// The full slice is the corpus subset.
    payloads: Vec<String>,
}

/// `Summary` stores the fields that the Rust benchmark reads from `summary.json`.
#[derive(Deserialize)]
struct Summary {
    corpus: String,
    freq_source: String,
    library: Vec<String>,
}

fn load_records() -> Result<Vec<Record>> {
    let path = results_dir().join("hover-payloads.jsonl");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!("reading {} - run tools/hover-parse-bench/payload.py first", path.display())
    })?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Record>(l).context("parsing a payload record"))
        .collect()
}

fn load_summary() -> Result<Summary> {
    let path = results_dir().join("summary.json");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!("reading {} - run tools/hover-parse-bench/payload.py first", path.display())
    })?;
    serde_json::from_str(&text).context("parsing summary.json")
}

// ---- statistics ----

/// `percentile` uses the nearest-rank percentile from `payload.py`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

/// `median` returns the median sample from one term. It sorts samples in place.
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
    /// Total payload bytes divided by total microseconds.
    ///
    /// This result is bytes per microsecond, which equals megabytes per second.
    mb_s: Option<f64>,
}

fn summarise(label: &str, mut micros: Vec<f64>, bytes: Option<f64>) -> Row {
    micros.sort_by(f64::total_cmp);
    let n = micros.len();
    let total: f64 = micros.iter().sum();
    Row {
        label: label.to_string(),
        n,
        mean: if n == 0 { 0.0 } else { total / n as f64 },
        p50: percentile(&micros, 0.50),
        p95: percentile(&micros, 0.95),
        p99: percentile(&micros, 0.99),
        max: micros.last().copied().unwrap_or(0.0),
        mb_s: bytes.filter(|_| total > 0.0).map(|b| b / total),
    }
}

fn print_table(title: &str, rows: &[Row]) {
    println!("\n{title}");
    let width = rows.iter().map(|r| r.label.chars().count()).max().unwrap_or(0).max(5);
    println!(
        "{:<width$}  {:>5}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>9}",
        "sample", "terms", "mean us", "p50 us", "p95 us", "p99 us", "max us", "MB/s"
    );
    println!("{}", "-".repeat(width + 2 + 5 + 6 * 12 + 3));
    for r in rows {
        let mb = r.mb_s.map_or_else(|| "-".to_string(), |v| format!("{v:.0}"));
        println!(
            "{:<width$}  {:>5}  {:>10.1}  {:>10.1}  {:>10.1}  {:>10.1}  {:>10.1}  {:>9}",
            r.label, r.n, r.mean, r.p50, r.p95, r.p99, r.max, mb
        );
    }
}

// ---- mode: parse ----

/// `walk` reads every node and every byte of each string and key.
///
/// The caller passes its result to `black_box`.
/// Without a consumed result, the optimizer can delete the walk.
/// A low-cost result can also let it delete most of the tree.
fn walk(v: &Value) -> usize {
    match v {
        Value::Null => 1,
        Value::Bool(_) => 1,
        Value::Number(_) => 1,
        Value::String(s) => 1 + s.len(),
        Value::Array(a) => 1 + a.iter().map(walk).sum::<usize>(),
        Value::Object(o) => 1 + o.iter().map(|(k, x)| k.len() + walk(x)).sum::<usize>(),
    }
}

/// `walk_doc` reads every node of a parsed `GlossDoc` and every byte of its text.
///
/// This function is the arena form of `walk`.
/// It makes one linear pass through the node vector.
/// The style matcher reads the vector in the same way.
fn walk_doc(doc: &GlossDoc) -> usize {
    doc.all_nodes()
        .iter()
        .enumerate()
        .map(|(i, n)| {
            1 + doc.text(i as u32).len()
                + doc.data(i as u32).len()
                + doc.style(i as u32).len()
                + usize::from(n.tag != Tag::None)
        })
        .sum()
}

/// `Rep` selects the representation that one timed pass builds.
#[derive(Clone, Copy)]
enum Rep {
    /// `Value` selects the representation measured in
    /// [hover-parse-cost.md](../docs/research/hover-parse-cost.md).
    /// This benchmark keeps it so it can reproduce those results.
    Value,
    /// `Typed` selects the shipped arena representation.
    Typed,
}

/// `time_parse` parses and walks every payload for one headword.
///
/// It repeats the pass until `MIN_SAMPLE` expires.
/// It returns the average time for one pass.
fn time_parse(rep: Rep, payloads: &[String]) -> f64 {
    let started = Instant::now();
    let mut iters: u32 = 0;
    loop {
        let mut acc = 0usize;
        for p in payloads {
            match rep {
                Rep::Value => {
                    let tree: Value =
                        serde_json::from_str(p).expect("payload.py wrote valid json");
                    acc = acc.wrapping_add(walk(&tree));
                }
                Rep::Typed => {
                    let doc = GlossDoc::parse(p);
                    acc = acc.wrapping_add(walk_doc(&doc));
                }
            }
        }
        black_box(acc);
        iters += 1;
        if started.elapsed() >= MIN_SAMPLE {
            break;
        }
    }
    started.elapsed().as_secs_f64() * 1e6 / f64::from(iters)
}

fn mode_parse() -> Result<()> {
    let records = load_records()?;
    if records.is_empty() {
        anyhow::bail!("hover-payloads.jsonl is empty");
    }
    println!(
        "parse: {} headwords, {} payloads, {:.1} MB of glossary json",
        records.len(),
        records.iter().map(|r| r.rows).sum::<usize>(),
        records.iter().map(|r| r.bytes).sum::<usize>() as f64 / 1e6,
    );
    // Both representations use the same payloads in the same run.
    // The `Value` row anchors the published results.
    // The `GlossDoc` row measures the shipped arena.
    parse_pass(&records, Rep::Value, "serde_json::Value + full walk")?;
    parse_pass(&records, Rep::Typed, "GlossDoc::parse + full walk")
}

fn parse_pass(records: &[Record], rep: Rep, label: &str) -> Result<()> {

    // Each headword has three slices.
    // Each slice answers a different part of the measurement:
    //
    // - `library` and `all 97` include every row that matches.
    //   A renderer parses this slice when it parses all rows for a headword.
    // - `top 10` is the slice that chibipop renders.
    //   `LookupEngine::run` truncates to `MAX_RESULTS` before it calls `entries`.
    //   One hover therefore deserializes no more than ten entries.
    //
    // `payload.py` writes payloads in library priority order.
    // For one surface form, this is engine rank order.
    // The order uses equal match length, equal form, equal headword frequency, and then
    // `dict_id` from low to high.
    // The first ten therefore represent the entries that the popup draws.
    //
    // Keep the frequency sample separate from the worst-case sample.
    // `payload.py` retains every worst-case headword that fits its limit.
    // These headwords make up about 2% of the file and are the most extreme in a
    // 97-archive corpus.
    // If the frequency sample included them, its p95 and p99 would approach the worst-case median.
    // Those percentiles would not describe the frequency sample.
    let mut freq_lib = Vec::new();
    let mut freq_lib_bytes = 0.0;
    let mut freq_all = Vec::new();
    let mut freq_all_bytes = 0.0;
    let mut freq_top = Vec::new();
    let mut freq_top_bytes = 0.0;
    let mut worst_lib = Vec::new();
    let mut worst_lib_bytes = 0.0;
    let mut worst_all = Vec::new();
    let mut worst_all_bytes = 0.0;
    let mut worst_top = Vec::new();
    let mut worst_top_bytes = 0.0;

    for rec in records {
        let us = time_parse(rep, &rec.payloads);
        if rec.worst {
            worst_all.push(us);
            worst_all_bytes += rec.bytes as f64;
        } else {
            freq_all.push(us);
            freq_all_bytes += rec.bytes as f64;
        }
        if rec.lib_rows > 0 {
            let us = time_parse(rep, &rec.payloads[..rec.lib_rows]);
            if rec.worst {
                worst_lib.push(us);
                worst_lib_bytes += rec.lib_bytes as f64;
            } else {
                freq_lib.push(us);
                freq_lib_bytes += rec.lib_bytes as f64;
            }
        }
        let top = &rec.payloads[..rec.payloads.len().min(MAX_RESULTS)];
        let bytes: usize = top.iter().map(String::len).sum();
        let us = time_parse(rep, top);
        if rec.worst {
            worst_top.push(us);
            worst_top_bytes += bytes as f64;
        } else {
            freq_top.push(us);
            freq_top_bytes += bytes as f64;
        }
    }

    print_table(
        &format!("structured-content parse cost per hover ({label})"),
        &[
            summarise("frequency, top 10 rendered", freq_top, Some(freq_top_bytes)),
            summarise("frequency, every library row", freq_lib, Some(freq_lib_bytes)),
            summarise("frequency, every row of 97", freq_all, Some(freq_all_bytes)),
            summarise("worst case, top 10 rendered", worst_top, Some(worst_top_bytes)),
            summarise("worst case, every library row", worst_lib, Some(worst_lib_bytes)),
            summarise("worst case, every row of 97", worst_all, Some(worst_all_bytes)),
        ],
    );
    Ok(())
}

// ---- mode: build ----

fn mode_build(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("usage: build <out.sqlite> [corpus]")?);
    let summary = load_summary()?;
    let corpus =
        PathBuf::from(args.get(1).cloned().unwrap_or_else(|| summary.corpus.clone()));

    // A temporary build must not write to the user's Dictionary library.
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && out.starts_with(Path::new(&home).join(".local/share/chibipop")) {
        anyhow::bail!("refusing to build into the real library: {}", out.display());
    }

    let terms: Vec<PathBuf> = summary.library.iter().map(|n| corpus.join(n)).collect();
    for t in &terms {
        if !t.exists() {
            anyhow::bail!("missing library archive {}", t.display());
        }
    }
    let freqs = vec![corpus.join(&summary.freq_source)];

    println!("build: {} term archives -> {}", terms.len(), out.display());
    for t in &terms {
        println!("  {}", t.file_name().unwrap_or_default().to_string_lossy());
    }
    println!("  freq: {}", summary.freq_source);

    let started = Instant::now();
    let last = std::cell::Cell::new(Instant::now());
    let counts = build(&terms, &freqs, &out, &|msg| {
        // The builder reports after each 5000 entries.
        // One line each second is enough.
        if last.get().elapsed() >= Duration::from_secs(1) {
            last.set(Instant::now());
            println!("  {msg}");
        }
    })?;
    let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "build: {} entries, {} term rows, {:.0} MB in {:.0}s",
        counts.entries,
        counts.terms,
        bytes as f64 / 1e6,
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

// ---- mode: hover ----

/// `Scale` uses a second read-only handle to measure glossary JSON bytes for one hover.
///
/// `length()` on TEXT counts characters.
/// The cast to BLOB makes it count bytes.
/// The mode opens this handle once outside the timed path.
struct Scale {
    conn: Connection,
}

impl Scale {
    fn open(db: &Path) -> Result<Scale> {
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {} read-only to weigh glossaries", db.display()))?;
        Ok(Scale { conn })
    }

    fn weigh(&self, ids: &[i64]) -> Result<usize> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT length(CAST(glossary AS BLOB)) FROM entry WHERE entry_id = ?1",
        )?;
        let mut total = 0usize;
        for id in ids {
            total += stmt.query_row([id], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
        }
        Ok(total)
    }
}

fn mode_hover(args: &[String]) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let default_db = format!("{home}/.local/share/chibipop/chibipop.sqlite");
    let db = PathBuf::from(args.first().cloned().unwrap_or(default_db));
    let label = args.get(1).cloned().unwrap_or_else(|| "hover".to_string());

    let dict = SqliteDictionary::open(&db)?;
    let names = dict.dicts()?;
    println!("hover: {} ({} dictionaries)", db.display(), names.len());
    for d in &names {
        println!("  {:>3}  {}", d.dict_id, d.name);
    }

    let records = load_records()?;

    // First, measure what one hover reads from this database.
    // Keep this pass outside the timed region.
    // This pass also warms the cache.
    //
    // Measure two shapes because `LookupEngine::run` sorts and truncates to
    // `MAX_RESULTS` before it calls `entries`.
    // `all` passes every matched entry ID.
    // `top` passes the first ten.
    // `top` measures the current hover cost.
    // `all` is the upper limit when the cap is removed.
    let scale = Scale::open(&db)?;
    let mut hits: Vec<(&str, bool)> = Vec::new();
    let mut total_entries = 0usize;
    let mut total_glossary = 0usize;
    let mut per_term_entries = Vec::new();
    let mut per_term_glossary = Vec::new();
    let mut per_term_top_glossary = Vec::new();
    let mut weighed: Vec<(f64, f64)> = Vec::new();
    for rec in &records {
        let rows = dict.terms_for(&rec.term)?;
        let ids: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
        let entries = dict.entries(&ids)?;
        if entries.is_empty() {
            continue;
        }
        let bytes = scale.weigh(&ids)?;
        let top_bytes = scale.weigh(&ids[..ids.len().min(MAX_RESULTS)])?;
        total_entries += entries.len();
        total_glossary += bytes;
        weighed.push((top_bytes as f64, bytes as f64));
        if !rec.worst {
            per_term_entries.push(entries.len() as f64);
            per_term_glossary.push(bytes as f64);
            per_term_top_glossary.push(top_bytes as f64);
        }
        hits.push((rec.term.as_str(), rec.worst));
    }
    if hits.is_empty() {
        anyhow::bail!("no sampled term matched anything in {}", db.display());
    }

    // Repeat the full list and keep the median sweep for each term.
    // Repeated calls to one term measure a cache state that an actual hover does not have.
    let mut top_samples: Vec<Vec<f64>> = vec![Vec::with_capacity(HOVER_SWEEPS); hits.len()];
    let mut all_samples: Vec<Vec<f64>> = vec![Vec::with_capacity(HOVER_SWEEPS); hits.len()];
    for _ in 0..HOVER_SWEEPS {
        for (i, (term, _)) in hits.iter().enumerate() {
            let started = Instant::now();
            let rows = dict.terms_for(term)?;
            let queried: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
            let entries = dict.entries(&queried[..queried.len().min(MAX_RESULTS)])?;
            let elapsed = started.elapsed();
            black_box(&entries);
            top_samples[i].push(elapsed.as_secs_f64() * 1e6);

            let started = Instant::now();
            let rows = dict.terms_for(term)?;
            let queried: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
            let entries = dict.entries(&queried)?;
            let elapsed = started.elapsed();
            black_box(&entries);
            all_samples[i].push(elapsed.as_secs_f64() * 1e6);
        }
    }

    // Use the same sample split as `parse`.
    // The retained file contains too many worst-case headwords for a frequency sample.
    // They distort the end of its distribution.
    let mut freq_top = (Vec::new(), 0.0);
    let mut freq_all = (Vec::new(), 0.0);
    let mut worst_top = (Vec::new(), 0.0);
    let mut worst_all = (Vec::new(), 0.0);
    for (i, (_, is_worst)) in hits.iter().enumerate() {
        let (top, all) = (median(&mut top_samples[i]), median(&mut all_samples[i]));
        let (top_bytes, all_bytes) = weighed[i];
        let (t, a) = if *is_worst {
            (&mut worst_top, &mut worst_all)
        } else {
            (&mut freq_top, &mut freq_all)
        };
        t.0.push(top);
        t.1 += top_bytes;
        a.0.push(all);
        a.1 += all_bytes;
    }

    print_table(
        &format!(
            "{label}: terms_for + entries over {} dictionaries \
             (includes the GlossDoc parse)",
            names.len()
        ),
        &[
            summarise("frequency, top 10 (real)", freq_top.0, Some(freq_top.1)),
            summarise("frequency, every match", freq_all.0, Some(freq_all.1)),
            summarise("worst case, top 10 (real)", worst_top.0, Some(worst_top.1)),
            summarise("worst case, every match", worst_all.0, Some(worst_all.1)),
        ],
    );
    print_table(
        &format!("{label}: what one hover deserializes today (frequency sample)"),
        &[
            summarise("entries matched per hover", per_term_entries, None),
            summarise("glossary bytes, top 10", per_term_top_glossary, None),
            summarise("glossary bytes, every match", per_term_glossary, None),
        ],
    );
    println!(
        "\n{label}: {} of {} sampled headwords hit; {} entries, {:.1} MB of glossary json total",
        hits.len(),
        records.len(),
        total_entries,
        total_glossary as f64 / 1e6,
    );
    Ok(())
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("parse") => mode_parse(),
        Some("build") => mode_build(&argv[1..]),
        Some("hover") => mode_hover(&argv[1..]),
        other => {
            anyhow::bail!(
                "unknown mode {:?}; expected parse | build <out.sqlite> [corpus] | \
                 hover [db] [label]",
                other.unwrap_or("<none>")
            )
        }
    }
}
