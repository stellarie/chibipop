//! What one hover costs, measured three ways.
//!
//! Behind the open question in the dictionary render-parity spec: the spec
//! parses structured content at *build* time and stores a typed tree, on the
//! stated grounds that "hover runs roughly 25 point queries and cannot afford
//! a parse". Storing the raw Yomitan JSON and parsing per hover keeps parser
//! bugs hot-fixable and costs no schema bump, so the choice turns entirely on
//! whether that parse actually fits in a hover. This measures it.
//!
//! Three modes, all printing a table to stdout:
//!
//! ```sh
//! cargo run -p chibipop --release --example hover_parse_bench -- parse
//! cargo run -p chibipop --release --example hover_parse_bench -- build <out.sqlite> [corpus]
//! cargo run -p chibipop --release --example hover_parse_bench -- hover [db] [label]
//! ```
//!
//! `parse` is the counterfactual: `serde_json` over the real glossary
//! payloads `tools/hover-parse-bench/payload.py` extracted, plus a full
//! recursive walk of each tree so the parse cannot be elided and the number
//! includes traversal cost comparable to building a typed tree.
//!
//! `build` writes a throwaway multi-dictionary database from the `library`
//! list in `results/summary.json` - the same twelve archives `payload.py`
//! measured its "realistic library" column over, so the two cannot drift.
//! It uses chibipop's own `dict::build::build`, and it must be pointed at a
//! scratch path: never the user's `~/.local/share/chibipop/chibipop.sqlite`.
//!
//! `hover` is the baseline: `terms_for` then `entries` against a database,
//! including the `Vec<Sense>` deserialization `entries` already does today.
//! It opens through `SqliteDictionary::open`, which passes
//! `SQLITE_OPEN_READ_ONLY` with no `SQLITE_OPEN_CREATE` and runs no
//! migration, so pointing it at the live database cannot modify it.
//!
//! Release mode is mandatory. A debug-profile serde number is meaningless.

use anyhow::{Context, Result};
use chibipop::dict::build::build;
use chibipop::lookup::engine::MAX_RESULTS;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Per-term timing floor. Short of this, `Instant` resolution and cache
/// noise dominate a single parse.
const MIN_SAMPLE: Duration = Duration::from_millis(50);

/// Sweeps over the whole term list in `hover` mode. Repeating one term back
/// to back would measure a cache that a real hover never has; sweeping the
/// list and taking the per-term median keeps the access pattern realistic.
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
    /// Library archives were scanned first, so `payloads[..lib_rows]` is the
    /// realistic-library subset and the whole slice is the all-corpus one.
    payloads: Vec<String>,
}

/// The half of `summary.json` the Rust side reads back.
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

/// Nearest-rank percentile, matching `payload.py`'s `percentile`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

/// The middle sweep of one term's samples. Sorts in place.
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
    /// Aggregate: total payload bytes divided by total microseconds, which
    /// is bytes per microsecond, which is megabytes per second.
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

/// Every node, and every byte of every string and key. Returned so the
/// caller can `black_box` it: without a consumed result the optimizer is
/// free to delete the walk, and with a cheap result it is free to delete
/// most of the tree.
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

/// One hover's worth of parsing: every payload behind one headword, parsed
/// and walked. Repeated until `MIN_SAMPLE` elapses, then averaged.
fn time_parse(payloads: &[String]) -> f64 {
    let started = Instant::now();
    let mut iters: u32 = 0;
    loop {
        let mut acc = 0usize;
        for p in payloads {
            let tree: Value = serde_json::from_str(p).expect("payload.py wrote valid json");
            acc = acc.wrapping_add(walk(&tree));
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

    // Three slices of every headword, because they answer different halves
    // of the question:
    //
    // - `library` / `all 97`: every matching row. This is what a renderer
    //   would parse if it parsed everything a headword matched.
    // - `top 10`: what chibipop actually renders. `LookupEngine::run`
    //   truncates to `MAX_RESULTS` before it ever calls `entries`, so no
    //   hover deserializes more than ten entries however many rows matched.
    //   `payload.py` writes payloads in library-priority order, which for a
    //   single surface form is the order the engine ranks them in: equal
    //   match length, equal form, equal headword frequency, then `dict_id`
    //   ascending. So the leading ten are a faithful stand-in for the ten
    //   the popup would draw.
    //
    // The frequency and worst-case samples stay apart. `payload.py` retains
    // every worst-case headword it can afford, so they sit at ~2% of the
    // file while being the most extreme headwords in a 97-archive corpus.
    // Folding them in would drag the frequency sample's p95 and p99 up to
    // the worst case's median and make the percentiles describe nothing.
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

    for rec in &records {
        let us = time_parse(&rec.payloads);
        if rec.worst {
            worst_all.push(us);
            worst_all_bytes += rec.bytes as f64;
        } else {
            freq_all.push(us);
            freq_all_bytes += rec.bytes as f64;
        }
        if rec.lib_rows > 0 {
            let us = time_parse(&rec.payloads[..rec.lib_rows]);
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
        let us = time_parse(top);
        if rec.worst {
            worst_top.push(us);
            worst_top_bytes += bytes as f64;
        } else {
            freq_top.push(us);
            freq_top_bytes += bytes as f64;
        }
    }

    print_table(
        "structured-content parse cost per hover (serde_json::Value + full walk)",
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

    // A throwaway build must never land on the user's library.
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
        // The builder reports every 5000 entries; one line a second is enough.
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

/// A second read-only handle, used only to weigh the `senses` JSON a hover
/// deserializes. `length()` on TEXT counts characters, so the cast to BLOB
/// is what makes this a byte count. Opened once: it is a measurement aid,
/// never on a timed path.
struct Scale {
    conn: Connection,
}

impl Scale {
    fn open(db: &Path) -> Result<Scale> {
        let conn = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening {} read-only to weigh senses", db.display()))?;
        Ok(Scale { conn })
    }

    fn weigh(&self, ids: &[i64]) -> Result<usize> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT length(CAST(senses AS BLOB)) FROM entry WHERE entry_id = ?1",
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

    // What one hover actually reads out of this database, measured once and
    // untimed. This also serves as the cache-warming pass.
    //
    // Two shapes, because `LookupEngine::run` sorts and then truncates to
    // `MAX_RESULTS` before it calls `entries`: `all` passes every matched
    // entry id, `top` passes the leading ten. `top` is what a hover pays
    // today; `all` is the ceiling if the cap were ever lifted.
    let scale = Scale::open(&db)?;
    let mut hits: Vec<(&str, bool)> = Vec::new();
    let mut total_entries = 0usize;
    let mut total_senses = 0usize;
    let mut per_term_entries = Vec::new();
    let mut per_term_senses = Vec::new();
    let mut per_term_top_senses = Vec::new();
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
        total_senses += bytes;
        weighed.push((top_bytes as f64, bytes as f64));
        if !rec.worst {
            per_term_entries.push(entries.len() as f64);
            per_term_senses.push(bytes as f64);
            per_term_top_senses.push(top_bytes as f64);
        }
        hits.push((rec.term.as_str(), rec.worst));
    }
    if hits.is_empty() {
        anyhow::bail!("no sampled term matched anything in {}", db.display());
    }

    // Sweep the whole list repeatedly and keep each term's median sweep.
    // Hammering one term would measure a cache no real hover enjoys.
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

    // Same split as `parse`: the worst-case headwords are deliberately
    // over-represented in the retained file and would poison the frequency
    // sample's tail.
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
             (includes today's Vec<Sense> parse)",
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
            summarise("senses bytes, top 10", per_term_top_senses, None),
            summarise("senses bytes, every match", per_term_senses, None),
        ],
    );
    println!(
        "\n{label}: {} of {} sampled headwords hit; {} entries, {:.1} MB of senses json total",
        hits.len(),
        records.len(),
        total_entries,
        total_senses as f64 / 1e6,
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
